use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::config::GatewayConfig;

// ── Trait + types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YieldPool {
    pub pool_id: String,
    pub protocol: String,
    pub chain: String,
    pub symbol: String,
    pub apy: f64,
    pub tvl_usd: f64,
    pub apy_7d: Option<f64>,
    pub apy_30d: Option<f64>,
    pub il_risk: Option<String>,
    pub source: String,
}

#[async_trait]
pub trait YieldDataProvider: Send + Sync {
    async fn fetch_pools(
        &self,
        chain: Option<&str>,
        min_tvl_usd: f64,
        min_apy: f64,
    ) -> Result<Vec<YieldPool>>;
}

// ── DefiLlama provider ──────────────────────────────────────────

pub struct DefiLlamaProvider {
    client: reqwest::Client,
    redis: Arc<redis::Client>,
}

impl DefiLlamaProvider {
    pub fn new(client: reqwest::Client, redis: Arc<redis::Client>) -> Self {
        Self { client, redis }
    }
}

/// Raw pool entry from the DeFi Llama /pools response.
#[derive(Deserialize)]
struct LlamaPool {
    pool: Option<String>,
    project: Option<String>,
    chain: Option<String>,
    symbol: Option<String>,
    apy: Option<f64>,
    #[serde(rename = "tvlUsd")]
    tvl_usd: Option<f64>,
    #[serde(rename = "apyBase7d")]
    apy_base_7d: Option<f64>,
    #[serde(rename = "apyMean30d")]
    apy_mean_30d: Option<f64>,
    #[serde(rename = "ilRisk")]
    il_risk: Option<String>,
}

#[derive(Deserialize)]
struct LlamaResponse {
    data: Vec<LlamaPool>,
}

#[async_trait]
impl YieldDataProvider for DefiLlamaProvider {
    async fn fetch_pools(
        &self,
        chain: Option<&str>,
        min_tvl_usd: f64,
        min_apy: f64,
    ) -> Result<Vec<YieldPool>> {
        let cache_key = format!(
            "vespra:yield_pools:{}",
            chain.unwrap_or("all")
        );

        // Check Redis cache
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let cached: Option<String> = conn.get(&cache_key).await.ok().flatten();
            if let Some(data) = cached {
                if let Ok(pools) = serde_json::from_str::<Vec<YieldPool>>(&data) {
                    // Apply filters on cached data (cache stores broader set)
                    let filtered: Vec<YieldPool> = pools
                        .into_iter()
                        .filter(|p| p.tvl_usd >= min_tvl_usd && p.apy >= min_apy)
                        .collect();
                    return Ok(filtered);
                }
            }
        }

        // Fetch from DeFi Llama
        let resp = self
            .client
            .get("https://yields.llama.fi/pools")
            .send()
            .await?;

        let llama: LlamaResponse = resp.json().await?;

        let chain_lower = chain.map(|c| c.to_lowercase());

        // Map and filter — cache with a looser filter (min_tvl 10k, min_apy 0.1)
        // so the cache is reusable across different query thresholds.
        let all_pools: Vec<YieldPool> = llama
            .data
            .into_iter()
            .filter_map(|p| {
                let pool_chain = p.chain.as_deref().unwrap_or("");
                if let Some(ref target) = chain_lower {
                    if pool_chain.to_lowercase() != *target {
                        return None;
                    }
                }
                let tvl = p.tvl_usd.unwrap_or(0.0);
                let apy = p.apy.unwrap_or(0.0);
                // Loose filter for caching
                if tvl < 10_000.0 || apy < 0.1 {
                    return None;
                }
                Some(YieldPool {
                    pool_id: p.pool.unwrap_or_default(),
                    protocol: p.project.unwrap_or_default(),
                    chain: pool_chain.to_string(),
                    symbol: p.symbol.unwrap_or_default(),
                    apy,
                    tvl_usd: tvl,
                    apy_7d: p.apy_base_7d,
                    apy_30d: p.apy_mean_30d,
                    il_risk: p.il_risk,
                    source: "defillama".to_string(),
                })
            })
            .collect();

        // Cache in Redis with 300s TTL
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            if let Ok(json) = serde_json::to_string(&all_pools) {
                let _: Result<(), _> = conn.set_ex(&cache_key, &json, 300).await;
            }
        }

        // Apply the caller's stricter filters
        let filtered: Vec<YieldPool> = all_pools
            .into_iter()
            .filter(|p| p.tvl_usd >= min_tvl_usd && p.apy >= min_apy)
            .collect();

        Ok(filtered)
    }
}

// ── Provider registry ───────────────────────────────────────────

pub struct ProviderRegistry {
    providers: Vec<Arc<dyn YieldDataProvider>>,
}

impl ProviderRegistry {
    pub fn from_config(
        config: &GatewayConfig,
        client: reqwest::Client,
        redis: Arc<redis::Client>,
    ) -> Self {
        let mut providers: Vec<Arc<dyn YieldDataProvider>> = Vec::new();

        for name in config.yield_providers.split(',').map(|s| s.trim()) {
            match name {
                "defillama" => {
                    providers.push(Arc::new(DefiLlamaProvider::new(
                        client.clone(),
                        redis.clone(),
                    )));
                    tracing::info!("yield provider registered: defillama");
                }
                "" => {}
                other => {
                    tracing::warn!("unknown yield provider '{other}', skipping");
                }
            }
        }

        if providers.is_empty() {
            tracing::warn!("no yield providers configured — Scout will have no live data");
        }

        Self { providers }
    }

    /// Fetch pools from all providers, merge, deduplicate by (protocol+chain+symbol),
    /// and sort by APY descending.
    pub async fn fetch_pools(
        &self,
        chain: Option<&str>,
        min_tvl_usd: f64,
        min_apy: f64,
    ) -> Result<Vec<YieldPool>> {
        if self.providers.is_empty() {
            return Ok(vec![]);
        }

        // Single provider fast path
        if self.providers.len() == 1 {
            return self.providers[0]
                .fetch_pools(chain, min_tvl_usd, min_apy)
                .await;
        }

        // Multi-provider: fetch concurrently, merge, deduplicate
        let mut handles = Vec::new();
        for provider in &self.providers {
            let p = provider.clone();
            let chain_owned = chain.map(|c| c.to_string());
            handles.push(tokio::spawn(async move {
                p.fetch_pools(chain_owned.as_deref(), min_tvl_usd, min_apy)
                    .await
            }));
        }

        let mut all_pools = Vec::new();
        for handle in handles {
            match handle.await {
                Ok(Ok(pools)) => all_pools.extend(pools),
                Ok(Err(e)) => tracing::warn!("yield provider fetch failed: {e}"),
                Err(e) => tracing::warn!("yield provider task panicked: {e}"),
            }
        }

        // Deduplicate by (protocol + chain + symbol) — keep highest APY
        let mut seen: HashSet<String> = HashSet::new();
        let mut deduped: HashMap<String, YieldPool> = HashMap::new();
        for pool in all_pools {
            let key = format!("{}:{}:{}", pool.protocol, pool.chain, pool.symbol);
            if !seen.contains(&key) {
                seen.insert(key.clone());
                deduped.insert(key, pool);
            } else if let Some(existing) = deduped.get(&key) {
                if pool.apy > existing.apy {
                    deduped.insert(key, pool);
                }
            }
        }

        let mut result: Vec<YieldPool> = deduped.into_values().collect();
        result.sort_by(|a, b| {
            b.apy
                .partial_cmp(&a.apy)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(result)
    }
}
