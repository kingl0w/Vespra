use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::Deserialize;

use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;
use crate::types::wallet::PriceData;

#[async_trait]
pub trait PriceOracle: Send + Sync {
    async fn fetch(&self, token_address: &str, chain: &str) -> Result<PriceData>;
    async fn fetch_batch(&self, tokens: &[(String, String)]) -> HashMap<String, PriceData>;
    fn name(&self) -> &str;
}

//─── defillama oracle ────────────────────────────────────────────

pub struct DefiLlamaOracle {
    client: reqwest::Client,
    redis: Arc<redis::Client>,
    chain_registry: Arc<ChainRegistry>,
}

impl DefiLlamaOracle {
    pub fn new(
        client: reqwest::Client,
        redis: Arc<redis::Client>,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self { client, redis, chain_registry }
    }

    fn resolve_slug(&self, chain: &str) -> Option<String> {
        self.chain_registry
            .get(chain)
            .map(|c| c.defillama_slug.clone())
    }
}

#[derive(Debug, Deserialize)]
struct LlamaCoinsResponse {
    coins: HashMap<String, LlamaCoinPrice>,
}

#[derive(Debug, Deserialize)]
struct LlamaCoinPrice {
    #[serde(default)]
    price: f64,
}

#[derive(Debug, Deserialize)]
struct LlamaChangeResponse {
    coins: HashMap<String, LlamaCoinChange>,
}

#[derive(Debug, Deserialize)]
struct LlamaCoinChange {
    #[serde(default)]
    change: Option<f64>,
}

#[async_trait]
impl PriceOracle for DefiLlamaOracle {
    async fn fetch(&self, token_address: &str, chain: &str) -> Result<PriceData> {
        let slug = match self.resolve_slug(chain) {
            Some(s) => s,
            None => {
                return Ok(PriceData {
                    source: "unknown_chain".into(),
                    ..PriceData::default()
                });
            }
        };

        //check redis cache
        let cache_key = format!(
            "vespra:price:{}:{}",
            chain.to_lowercase(),
            token_address.to_lowercase()
        );
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let cached: Option<String> = conn.get(&cache_key).await.ok().flatten();
            if let Some(data) = cached {
                if let Ok(pd) = serde_json::from_str::<PriceData>(&data) {
                    return Ok(pd);
                }
            }
        }

        let coin_id = format!("{slug}:{token_address}");

        //fetch price
        let price_url = format!("https://coins.llama.fi/prices/current/{coin_id}");
        let price_resp = self
            .client
            .get(&price_url)
            .send()
            .await
            .context("defillama price request failed")?
            .json::<LlamaCoinsResponse>()
            .await
            .context("defillama price parse failed")?;

        let price_usd = price_resp
            .coins
            .get(&coin_id)
            .map(|c| c.price)
            .unwrap_or(0.0);

        //fetch 24h change
        let change_url = format!(
            "https://coins.llama.fi/percentage/{coin_id}?period=24h"
        );
        let change_pct = match self.client.get(&change_url).send().await {
            Ok(resp) => resp
                .json::<LlamaChangeResponse>()
                .await
                .ok()
                .and_then(|r| r.coins.get(&coin_id).and_then(|c| c.change))
                .unwrap_or(0.0),
            Err(_) => 0.0,
        };

        let now = chrono::Utc::now().timestamp();
        let price_data = PriceData {
            price_usd,
            price_change_24h_pct: change_pct,
            source: "defillama".into(),
            timestamp: now,
        };

        //cache with 300s ttl
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            if let Ok(json) = serde_json::to_string(&price_data) {
                let _: Result<(), _> = conn.set_ex(&cache_key, &json, 300).await;
            }
        }

        Ok(price_data)
    }

    async fn fetch_batch(&self, tokens: &[(String, String)]) -> HashMap<String, PriceData> {
        let mut result = HashMap::new();

        //group tokens by chain → defillama slug
        let mut by_slug: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (addr, chain) in tokens {
            match self.resolve_slug(chain) {
                Some(slug) => by_slug.entry(slug).or_default().push((addr.clone(), chain.clone())),
                None => {
                    result.insert(
                        addr.clone(),
                        PriceData {
                            source: "unknown_chain".into(),
                            ..PriceData::default()
                        },
                    );
                }
            }
        }

        for (slug, addrs) in &by_slug {
            let coin_ids: Vec<String> = addrs
                .iter()
                .map(|(addr, _)| format!("{slug}:{addr}"))
                .collect();
            let joined = coin_ids.join(",");
            let url = format!("https://coins.llama.fi/prices/current/{joined}");

            let resp = match self.client.get(&url).send().await {
                Ok(r) => r.json::<LlamaCoinsResponse>().await.ok(),
                Err(_) => None,
            };

            let now = chrono::Utc::now().timestamp();
            for (addr, _chain) in addrs {
                let coin_id = format!("{slug}:{addr}");
                let price_usd = resp
                    .as_ref()
                    .and_then(|r| r.coins.get(&coin_id))
                    .map(|c| c.price)
                    .unwrap_or(0.0);
                result.insert(
                    addr.clone(),
                    PriceData {
                        price_usd,
                        price_change_24h_pct: 0.0,
                        source: "defillama".into(),
                        timestamp: now,
                    },
                );
            }
        }

        result
    }

    fn name(&self) -> &str {
        "defillama"
    }
}

//─── onchain twap oracle ─────────────────────────────────────────

pub struct OnchainTwapOracle {
    chain_registry: Arc<ChainRegistry>,
}

impl OnchainTwapOracle {
    pub fn new(chain_registry: Arc<ChainRegistry>) -> Self {
        Self { chain_registry }
    }
}

#[async_trait]
impl PriceOracle for OnchainTwapOracle {
    async fn fetch(&self, _token_address: &str, chain: &str) -> Result<PriceData> {
        let cfg = match self.chain_registry.get(chain) {
            Some(c) => c,
            None => {
                return Ok(PriceData {
                    source: "unknown_chain".into(),
                    ..PriceData::default()
                });
            }
        };

        if cfg.rpc_url.is_empty() {
            return Ok(PriceData {
                source: "no_rpc".into(),
                ..PriceData::default()
            });
        }

        //phase 6.2 — twap via alloy eth_call to pool observe() (not yet implemented)
        Ok(PriceData {
            source: "onchain_twap_pending".into(),
            ..PriceData::default()
        })
    }

    async fn fetch_batch(&self, tokens: &[(String, String)]) -> HashMap<String, PriceData> {
        let mut result = HashMap::new();
        for (addr, chain) in tokens {
            let pd = self.fetch(addr, chain).await.unwrap_or_default();
            result.insert(addr.clone(), pd);
        }
        result
    }

    fn name(&self) -> &str {
        "onchain_twap"
    }
}

//─── coingecko oracle ────────────────────────────────────────────

pub struct CoinGeckoOracle {
    client: reqwest::Client,
    api_key: Option<String>,
    chain_registry: Arc<ChainRegistry>,
}

impl CoinGeckoOracle {
    pub fn new(
        client: reqwest::Client,
        api_key: Option<String>,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self { client, api_key, chain_registry }
    }
}

#[derive(Debug, Deserialize)]
struct CoinGeckoContract {
    market_data: Option<CoinGeckoMarket>,
}

#[derive(Debug, Deserialize)]
struct CoinGeckoMarket {
    current_price: Option<HashMap<String, f64>>,
    price_change_percentage_24h: Option<f64>,
}

#[async_trait]
impl PriceOracle for CoinGeckoOracle {
    async fn fetch(&self, token_address: &str, chain: &str) -> Result<PriceData> {
        let coingecko_id = match self.chain_registry.get(chain) {
            Some(c) => c.coingecko_id.clone(),
            None => {
                return Ok(PriceData {
                    source: "unknown_chain".into(),
                    ..PriceData::default()
                });
            }
        };

        let url = format!(
            "https://api.coingecko.com/api/v3/coins/{coingecko_id}/contract/{token_address}"
        );

        let mut req = self.client.get(&url);
        if let Some(ref key) = self.api_key {
            req = req.header("x-cg-demo-api-key", key);
        }

        let resp = req
            .send()
            .await
            .context("coingecko request failed")?
            .json::<CoinGeckoContract>()
            .await
            .context("coingecko parse failed")?;

        let now = chrono::Utc::now().timestamp();
        let market = resp.market_data.as_ref();
        let price_usd = market
            .and_then(|m| m.current_price.as_ref())
            .and_then(|p| p.get("usd"))
            .copied()
            .unwrap_or(0.0);
        let change_pct = market
            .and_then(|m| m.price_change_percentage_24h)
            .unwrap_or(0.0);

        Ok(PriceData {
            price_usd,
            price_change_24h_pct: change_pct,
            source: "coingecko".into(),
            timestamp: now,
        })
    }

    async fn fetch_batch(&self, tokens: &[(String, String)]) -> HashMap<String, PriceData> {
        let mut result = HashMap::new();
        for (addr, chain) in tokens {
            let pd = self.fetch(addr, chain).await.unwrap_or_default();
            result.insert(addr.clone(), pd);
        }
        result
    }

    fn name(&self) -> &str {
        "coingecko"
    }
}

//─── custom oracle ───────────────────────────────────────────────

pub struct CustomOracle {
    client: reqwest::Client,
    base_url: String,
    chain_registry: Arc<ChainRegistry>,
}

impl CustomOracle {
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self { client, base_url, chain_registry }
    }
}

#[derive(Debug, Deserialize)]
struct CustomPriceResponse {
    #[serde(default)]
    price_usd: f64,
    #[serde(default)]
    price_change_24h_pct: f64,
}

#[async_trait]
impl PriceOracle for CustomOracle {
    async fn fetch(&self, token_address: &str, chain: &str) -> Result<PriceData> {
        if self.base_url.is_empty() {
            return Ok(PriceData::default());
        }

        let chain_id = self
            .chain_registry
            .get(chain)
            .map(|c| c.chain_id)
            .unwrap_or(0);

        let url = format!(
            "{}/price?token={token_address}&chain={chain}&chain_id={chain_id}",
            self.base_url.trim_end_matches('/')
        );

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("custom oracle request failed")?
            .json::<CustomPriceResponse>()
            .await
            .context("custom oracle parse failed")?;

        Ok(PriceData {
            price_usd: resp.price_usd,
            price_change_24h_pct: resp.price_change_24h_pct,
            source: "custom".into(),
            timestamp: chrono::Utc::now().timestamp(),
        })
    }

    async fn fetch_batch(&self, tokens: &[(String, String)]) -> HashMap<String, PriceData> {
        let mut result = HashMap::new();
        for (addr, chain) in tokens {
            let pd = self.fetch(addr, chain).await.unwrap_or_default();
            result.insert(addr.clone(), pd);
        }
        result
    }

    fn name(&self) -> &str {
        "custom"
    }
}

//─── oracle router ───────────────────────────────────────────────

pub struct OracleRouter {
    primary: Box<dyn PriceOracle>,
    fallback: Option<Box<dyn PriceOracle>>,
}

impl OracleRouter {
    pub fn from_config(
        config: &GatewayConfig,
        chain_registry: Arc<ChainRegistry>,
        redis: Arc<redis::Client>,
        client: reqwest::Client,
    ) -> Self {
        let primary = build_oracle(
            &config.price_oracle,
            client.clone(),
            redis.clone(),
            chain_registry.clone(),
            config,
        );

        let fallback = match config.price_oracle_fallback.as_str() {
            "none" | "" => None,
            name => Some(build_oracle(
                name,
                client,
                redis,
                chain_registry,
                config,
            )),
        };

        Self { primary, fallback }
    }
}

fn build_oracle(
    name: &str,
    client: reqwest::Client,
    redis: Arc<redis::Client>,
    chain_registry: Arc<ChainRegistry>,
    config: &GatewayConfig,
) -> Box<dyn PriceOracle> {
    match name {
        "defillama" => Box::new(DefiLlamaOracle::new(client, redis, chain_registry)),
        "onchain_twap" => Box::new(OnchainTwapOracle::new(chain_registry)),
        "coingecko" => Box::new(CoinGeckoOracle::new(
            client,
            config.price_oracle_api_key.clone(),
            chain_registry,
        )),
        "custom" => Box::new(CustomOracle::new(
            client,
            config.price_oracle_base_url.clone().unwrap_or_default(),
            chain_registry,
        )),
        other => {
            tracing::warn!("Unknown price oracle '{other}', defaulting to defillama");
            Box::new(DefiLlamaOracle::new(client, redis, chain_registry))
        }
    }
}

#[async_trait]
impl PriceOracle for OracleRouter {
    async fn fetch(&self, token_address: &str, chain: &str) -> Result<PriceData> {
        match self.primary.fetch(token_address, chain).await {
            Ok(pd) if pd.price_usd > 0.0 => return Ok(pd),
            _ => {}
        }

        if let Some(ref fb) = self.fallback {
            match fb.fetch(token_address, chain).await {
                Ok(pd) => return Ok(pd),
                Err(_) => {}
            }
        }

        Ok(PriceData::default())
    }

    async fn fetch_batch(&self, tokens: &[(String, String)]) -> HashMap<String, PriceData> {
        let mut result = self.primary.fetch_batch(tokens).await;

        //find tokens with no price from primary
        if let Some(ref fb) = self.fallback {
            let missing: Vec<(String, String)> = tokens
                .iter()
                .filter(|(addr, _)| {
                    result
                        .get(addr)
                        .map(|pd| pd.price_usd == 0.0)
                        .unwrap_or(true)
                })
                .cloned()
                .collect();

            if !missing.is_empty() {
                let fb_result = fb.fetch_batch(&missing).await;
                for (addr, pd) in fb_result {
                    if pd.price_usd > 0.0 {
                        result.insert(addr, pd);
                    }
                }
            }
        }

        result
    }

    fn name(&self) -> &str {
        "router"
    }
}
