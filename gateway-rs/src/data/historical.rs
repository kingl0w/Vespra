
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApySnapshot {
    pub date: NaiveDate,
    pub apy: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub date: NaiveDate,
    pub price_usd: f64,
}

#[async_trait]
pub trait HistoricalFeed: Send + Sync {
    ///daily apy snapshots for the given defillama pool id. inclusive on both ends.
    async fn apy_series(
        &self,
        pool_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ApySnapshot>>;

    async fn price_series(
        &self,
        coingecko_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<PriceSnapshot>>;
}

//─── rate-limit guard ──────────────────────────────────────────────────

#[derive(Debug)]
struct RateGate {
    min_interval_ms: u64,
    last_call: Mutex<Option<std::time::Instant>>,
}

impl RateGate {
    fn new(min_interval_ms: u64) -> Self {
        Self {
            min_interval_ms,
            last_call: Mutex::new(None),
        }
    }

    async fn wait(&self) {
        let mut last = self.last_call.lock().await;
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            let min = std::time::Duration::from_millis(self.min_interval_ms);
            if elapsed < min {
                tokio::time::sleep(min - elapsed).await;
            }
        }
        *last = Some(std::time::Instant::now());
    }
}

//─── defillama ─────────────────────────────────────────────────────────

pub struct DeFiLlamaHistorical {
    client: reqwest::Client,
    rate_gate: Arc<RateGate>,
}

impl DeFiLlamaHistorical {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            rate_gate: Arc::new(RateGate::new(300)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DeFiLlamaChartResponse {
    #[serde(default)]
    data: Vec<DeFiLlamaChartPoint>,
}

#[derive(Debug, Deserialize)]
struct DeFiLlamaChartPoint {
    ///iso-8601 timestamp string (e.g. "2024-03-01t00:00:00.000z").
    timestamp: String,
    #[serde(default)]
    apy: Option<f64>,
}

#[async_trait]
impl HistoricalFeed for DeFiLlamaHistorical {
    async fn apy_series(
        &self,
        pool_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ApySnapshot>> {
        self.rate_gate.wait().await;

        let url = format!("https://yields.llama.fi/chart/{pool_id}");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("DeFiLlama GET {url} failed"))?
            .error_for_status()
            .with_context(|| format!("DeFiLlama returned non-2xx for pool {pool_id}"))?;

        let body: DeFiLlamaChartResponse = resp
            .json()
            .await
            .with_context(|| format!("DeFiLlama JSON parse failed for pool {pool_id}"))?;

        let mut out: Vec<ApySnapshot> = Vec::new();
        for point in body.data {
            let Some(apy) = point.apy else { continue };
            let parsed: DateTime<Utc> = match point.timestamp.parse::<DateTime<Utc>>() {
                Ok(ts) => ts,
                Err(_) => continue,
            };
            let date = parsed.date_naive();
            if date >= from && date <= to {
                out.push(ApySnapshot { date, apy });
            }
        }
        out.sort_by_key(|s| s.date);
        Ok(out)
    }

    async fn price_series(
        &self,
        _coingecko_id: &str,
        _from: NaiveDate,
        _to: NaiveDate,
    ) -> Result<Vec<PriceSnapshot>> {
        anyhow::bail!("DeFiLlamaHistorical does not provide price series — use CoinGeckoHistorical")
    }
}

//─── coingecko ─────────────────────────────────────────────────────────

pub struct CoinGeckoHistorical {
    client: reqwest::Client,
    rate_gate: Arc<RateGate>,
}

impl CoinGeckoHistorical {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            rate_gate: Arc::new(RateGate::new(300)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CoinGeckoMarketChart {
    #[serde(default)]
    prices: Vec<[f64; 2]>,
}

#[async_trait]
impl HistoricalFeed for CoinGeckoHistorical {
    async fn apy_series(
        &self,
        _pool_id: &str,
        _from: NaiveDate,
        _to: NaiveDate,
    ) -> Result<Vec<ApySnapshot>> {
        anyhow::bail!("CoinGeckoHistorical does not provide APY series — use DeFiLlamaHistorical")
    }

    async fn price_series(
        &self,
        coingecko_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<PriceSnapshot>> {
        self.rate_gate.wait().await;

        //convert to inclusive utc unix bounds. coingecko expects seconds.
        let from_ts = Utc
            .from_utc_datetime(&from.and_hms_opt(0, 0, 0).unwrap())
            .timestamp();
        let to_ts = Utc
            .from_utc_datetime(&to.and_hms_opt(23, 59, 59).unwrap())
            .timestamp();

        let url = format!(
            "https://api.coingecko.com/api/v3/coins/{coingecko_id}/market_chart/range?vs_currency=usd&from={from_ts}&to={to_ts}"
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("CoinGecko GET {url} failed"))?
            .error_for_status()
            .with_context(|| format!("CoinGecko returned non-2xx for coin {coingecko_id}"))?;

        let body: CoinGeckoMarketChart = resp
            .json()
            .await
            .with_context(|| format!("CoinGecko JSON parse failed for coin {coingecko_id}"))?;

        //coingecko returns multiple intra-day samples; collapse to last value per day.
        let mut by_day: std::collections::BTreeMap<NaiveDate, f64> =
            std::collections::BTreeMap::new();
        for [ts_ms, price] in body.prices {
            let secs = (ts_ms / 1000.0) as i64;
            let Some(dt) = Utc.timestamp_opt(secs, 0).single() else {
                continue;
            };
            let date = dt.date_naive();
            if date >= from && date <= to {
                by_day.insert(date, price);
            }
        }

        Ok(by_day
            .into_iter()
            .map(|(date, price_usd)| PriceSnapshot { date, price_usd })
            .collect())
    }
}

//─── composite (default wiring) ────────────────────────────────────────

///combines a defillama apy source and a coingecko price source into a single
///[`historicalfeed`]. this is what the runner sees through `appstate`.
pub struct CompositeHistoricalFeed {
    apy: Arc<DeFiLlamaHistorical>,
    price: Arc<CoinGeckoHistorical>,
}

impl CompositeHistoricalFeed {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            apy: Arc::new(DeFiLlamaHistorical::new(client.clone())),
            price: Arc::new(CoinGeckoHistorical::new(client)),
        }
    }
}

#[async_trait]
impl HistoricalFeed for CompositeHistoricalFeed {
    async fn apy_series(
        &self,
        pool_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<ApySnapshot>> {
        self.apy.apy_series(pool_id, from, to).await
    }

    async fn price_series(
        &self,
        coingecko_id: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<PriceSnapshot>> {
        self.price.price_series(coingecko_id, from, to).await
    }
}
