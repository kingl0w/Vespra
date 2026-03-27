use std::sync::Arc;

use anyhow::{Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::chain::ChainRegistry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolData {
    pub tvl: f64,
    pub tvl_trend_pct: f64,
    pub tvl_velocity_1hr: f64,
    pub audits: Vec<String>,
    pub age_days: u64,
    pub liquidity_locked: bool,
    pub chain: String,
}

impl Default for ProtocolData {
    fn default() -> Self {
        Self {
            tvl: 0.0,
            tvl_trend_pct: 0.0,
            tvl_velocity_1hr: 0.0,
            audits: vec![],
            age_days: 0,
            liquidity_locked: false,
            chain: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LlamaProtocol {
    #[serde(default)]
    tvl: Vec<TvlEntry>,
    #[serde(default)]
    audits: Option<serde_json::Value>,
    #[serde(default)]
    audit_links: Option<Vec<String>>,
    #[serde(default)]
    listed_at: Option<u64>,
    #[serde(default)]
    chains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct TvlEntry {
    #[serde(default)]
    date: u64,
    #[serde(default, rename = "totalLiquidityUSD")]
    total_liquidity_usd: f64,
}

pub struct ProtocolFetcher {
    client: reqwest::Client,
    redis: Arc<redis::Client>,
    chain_registry: Arc<ChainRegistry>,
}

impl ProtocolFetcher {
    pub fn new(
        client: reqwest::Client,
        redis: Arc<redis::Client>,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self { client, redis, chain_registry }
    }

    pub async fn fetch_protocol(&self, slug: &str) -> Result<ProtocolData> {
        let cache_key = format!("vespra:protocol:{slug}");

        // Check cache
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let cached: Option<String> = conn.get(&cache_key).await.ok().flatten();
            if let Some(data) = cached {
                if let Ok(pd) = serde_json::from_str::<ProtocolData>(&data) {
                    return Ok(pd);
                }
            }
        }

        let url = format!("https://api.llama.fi/protocol/{slug}");
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to fetch llama protocol")?
            .json::<LlamaProtocol>()
            .await
            .context("failed to parse llama protocol response")?;

        // Current TVL — last entry in tvl array
        let current_tvl = resp
            .tvl
            .last()
            .map(|e| e.total_liquidity_usd)
            .unwrap_or(0.0);

        // TVL trend — 30-day change
        let tvl_trend_pct =
            if let (Some(latest), true) = (resp.tvl.last(), resp.tvl.len() >= 2) {
                let target_ts = latest.date.saturating_sub(30 * 86400);
                let past_val = resp
                    .tvl
                    .iter()
                    .filter(|e| e.date <= target_ts)
                    .last()
                    .map(|e| e.total_liquidity_usd)
                    .unwrap_or(0.0);
                if past_val > 0.0 {
                    ((latest.total_liquidity_usd - past_val) / past_val) * 100.0
                } else {
                    0.0
                }
            } else {
                0.0
            };

        // TVL velocity — compare current to 1hr-ago cached value
        let velocity_key = format!("vespra:tvl_cache:{slug}");
        let tvl_velocity_1hr = if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let prev: Option<String> = conn.get(&velocity_key).await.ok().flatten();
            let velocity = match prev {
                Some(s) => {
                    let prev_tvl: f64 = s.parse().unwrap_or(0.0);
                    if prev_tvl > 0.0 {
                        ((current_tvl - prev_tvl) / prev_tvl) * 100.0
                    } else {
                        0.0
                    }
                }
                None => 0.0,
            };
            // Store current TVL with 1hr TTL for next comparison
            let _: Result<(), _> = conn
                .set_ex(&velocity_key, current_tvl.to_string(), 3600)
                .await;
            velocity
        } else {
            0.0
        };

        // Age in days from first TVL entry
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let age_days = resp
            .tvl
            .first()
            .map(|e| if e.date > 0 { (now - e.date) / 86400 } else { 0 })
            .unwrap_or(0);

        // Audits
        let audits = match resp.audit_links {
            Some(links) => links,
            None => match resp.audits {
                Some(serde_json::Value::Array(arr)) => arr
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect(),
                _ => vec![],
            },
        };

        // Resolve chain from response "chains" array via ChainRegistry
        let chain = resp
            .chains
            .as_ref()
            .and_then(|chains| {
                chains.iter().find_map(|slug_raw| {
                    self.chain_registry
                        .from_defillama_slug(slug_raw)
                        .map(|c| c.name.clone())
                })
            })
            .unwrap_or_default();

        let data = ProtocolData {
            tvl: current_tvl,
            tvl_trend_pct,
            tvl_velocity_1hr,
            audits,
            age_days,
            liquidity_locked: resp.listed_at.is_some(),
            chain,
        };

        // Cache with 300s TTL
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            if let Ok(json) = serde_json::to_string(&data) {
                let _: Result<(), _> = conn.set_ex(&cache_key, &json, 300).await;
            }
        }

        Ok(data)
    }
}
