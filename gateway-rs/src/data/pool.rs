use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use redis::AsyncCommands;

use crate::chain::ChainRegistry;
use crate::types::opportunity::{EntrySignal, Opportunity, RiskTier};

pub struct PoolFetcher {
    client: reqwest::Client,
    redis: Arc<redis::Client>,
    chain_registry: Arc<ChainRegistry>,
}

impl PoolFetcher {
    pub fn new(
        client: reqwest::Client,
        redis: Arc<redis::Client>,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self { client, redis, chain_registry }
    }

    /// Fetch top opportunities for the given chain names.
    pub async fn fetch(&self, chains: &[String]) -> Result<Vec<Opportunity>> {
        self.fetch_inner(chains, None).await
    }

    /// Fetch opportunities filtered to specific protocols.
    pub async fn fetch_for_protocol(
        &self,
        chains: &[String],
        protocols: &[String],
    ) -> Result<Vec<Opportunity>> {
        self.fetch_inner(chains, Some(protocols)).await
    }

    async fn fetch_inner(
        &self,
        chains: &[String],
        protocol_filter: Option<&[String]>,
    ) -> Result<Vec<Opportunity>> {
        // Resolve chain names → defillama slugs via ChainRegistry.
        // slug_to_name maps lowercase defillama slug → our chain name.
        let mut slug_to_name: HashMap<String, String> = HashMap::new();
        for name in chains {
            match self.chain_registry.get(name) {
                Some(cfg) => {
                    slug_to_name.insert(cfg.defillama_slug.to_lowercase(), cfg.name.clone());
                }
                None => {
                    tracing::warn!("chain '{name}' not found in registry, skipping");
                }
            }
        }
        if slug_to_name.is_empty() {
            return Ok(vec![]);
        }

        // Cache key: sorted chain names joined by ","
        let mut sorted_chains: Vec<&str> = slug_to_name.values().map(|s| s.as_str()).collect();
        sorted_chains.sort();
        let cache_key = format!("vespra:pools:{}", sorted_chains.join(","));

        // Check Redis cache
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let cached: Option<String> = conn.get(&cache_key).await.ok().flatten();
            if let Some(data) = cached {
                if let Ok(mut opps) = serde_json::from_str::<Vec<Opportunity>>(&data) {
                    if let Some(protos) = protocol_filter {
                        let proto_set: std::collections::HashSet<String> =
                            protos.iter().map(|p| p.to_lowercase()).collect();
                        opps.retain(|o| proto_set.contains(&o.protocol));
                    }
                    return Ok(opps);
                }
            }
        }

        // Fetch from DeFi Llama
        let resp = self.client
            .get("https://yields.llama.fi/pools")
            .send()
            .await
            .context("failed to fetch llama pools")?;

        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse llama pools response")?;

        let items = body.get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        let mut opps = Vec::new();

        for item in &items {
            // Match chain via defillama_slug from ChainRegistry
            let pool_chain_raw = item.get("chain").and_then(|v| v.as_str()).unwrap_or("");
            let pool_chain_lower = pool_chain_raw.to_lowercase();

            let chain_name = match slug_to_name.get(&pool_chain_lower) {
                Some(name) => name.clone(),
                None => {
                    // Also try reverse lookup through ChainRegistry for unusual casing
                    match self.chain_registry.from_defillama_slug(pool_chain_raw) {
                        Some(cfg) if slug_to_name.contains_key(&cfg.defillama_slug.to_lowercase()) => {
                            cfg.name.clone()
                        }
                        _ => continue,
                    }
                }
            };

            let apy = item.get("apy").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tvl_usd_f = item.get("tvlUsd").and_then(|v| v.as_f64()).unwrap_or(0.0);

            // Filter: tvl > 100k, apy > 0
            if tvl_usd_f <= 100_000.0 || apy <= 0.0 {
                continue;
            }

            let protocol = item.get("project")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let pool = item.get("symbol")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let volume_24h_f = item.get("volumeUsd1d").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tvl_usd = tvl_usd_f as u64;
            let volume_24h = volume_24h_f as u64;

            let volume_spike_pct = if tvl_usd > 0 {
                (volume_24h_f / tvl_usd_f * 100.0).min(1000.0)
            } else {
                0.0
            };

            let momentum_score =
                (volume_spike_pct / 1000.0) * 0.5 + (apy / 10000.0).min(0.5);

            let entry_signal = if momentum_score >= 0.7 {
                EntrySignal::Strong
            } else if momentum_score >= 0.5 {
                EntrySignal::Moderate
            } else if momentum_score >= 0.3 {
                EntrySignal::Weak
            } else {
                EntrySignal::None
            };

            let risk_tier = if apy > 50.0 {
                RiskTier::High
            } else if apy > 10.0 {
                RiskTier::Medium
            } else {
                RiskTier::Low
            };

            let il_risk = item.get("il7d")
                .and_then(|v| v.as_f64())
                .map(|v| v != 0.0)
                .unwrap_or(false);

            opps.push(Opportunity {
                protocol,
                pool,
                chain: chain_name,
                apy,
                tvl_usd,
                momentum_score,
                entry_signal,
                price_change_24h_pct: 0.0,
                price_usd: 0.0,
                risk_tier,
                il_risk,
                volume_24h,
                volume_spike_pct,
                tvl_change_7d_pct: 0.0,
            });
        }

        // Sort by momentum_score descending, take top 50
        opps.sort_by(|a, b| {
            b.momentum_score
                .partial_cmp(&a.momentum_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        opps.truncate(50);

        // Cache in Redis with 60s TTL
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            if let Ok(json) = serde_json::to_string(&opps) {
                let _: Result<(), _> = conn.set_ex(&cache_key, &json, 60).await;
            }
        }

        // Apply protocol filter if requested
        if let Some(protos) = protocol_filter {
            let proto_set: std::collections::HashSet<String> =
                protos.iter().map(|p| p.to_lowercase()).collect();
            opps.retain(|o| proto_set.contains(&o.protocol));
        }

        Ok(opps)
    }
}
