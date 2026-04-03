use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::executor::ExecutorAgent;
use crate::agents::risk::{RiskAgent, RiskContext};
use crate::agents::sniper::{SniperAgent, SniperContext};
use crate::config::GatewayConfig;
use crate::data::protocol::ProtocolFetcher;
use crate::data::quote::QuoteFetcher;
use crate::chain::ChainRegistry;
use crate::types::decisions::{ExecutorStatus, SniperDecision};

// ─── Types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolEvent {
    pub pool_address: String,
    pub token0: String,
    pub token1: String,
    pub tvl_usd: f64,
    pub protocol: String,
    pub chain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperEntry {
    pub position_id: String,
    pub pool_address: String,
    pub token_in: String,
    pub token_out: String,
    pub amount_eth: f64,
    pub tx_hash: Option<String>,
    pub chain: String,
    pub protocol: String,
    pub confidence: f64,
    pub status: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperResult {
    pub action: String,
    pub position_id: Option<String>,
    pub reason: String,
    pub tx_hash: Option<String>,
}

// ─── Orchestrator ────────────────────────────────────────────────

pub struct SniperOrchestrator {
    risk: Arc<RiskAgent>,
    pub sniper: Arc<SniperAgent>,
    executor: Arc<ExecutorAgent>,
    protocol_fetcher: Arc<ProtocolFetcher>,
    quote_fetcher: Arc<QuoteFetcher>,
    chain_registry: Arc<ChainRegistry>,
    config: Arc<GatewayConfig>,
    redis: Arc<redis::Client>,
    kill_flag: Arc<AtomicBool>,
}

impl SniperOrchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        risk: Arc<RiskAgent>,
        sniper: Arc<SniperAgent>,
        executor: Arc<ExecutorAgent>,
        protocol_fetcher: Arc<ProtocolFetcher>,
        quote_fetcher: Arc<QuoteFetcher>,
        chain_registry: Arc<ChainRegistry>,
        config: Arc<GatewayConfig>,
        redis: Arc<redis::Client>,
        kill_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            risk,
            sniper,
            executor,
            protocol_fetcher,
            quote_fetcher,
            chain_registry,
            config,
            redis,
            kill_flag,
        }
    }

    pub fn is_killed(&self) -> bool {
        self.kill_flag.load(Ordering::SeqCst)
    }

    pub async fn evaluate_pool(&self, event: PoolEvent, wallet_id: Uuid) -> SniperResult {
        // Gate checks
        if self.is_killed() {
            return SniperResult {
                action: "skipped".into(),
                position_id: None,
                reason: "kill_switch_active".into(),
                tx_hash: None,
            };
        }

        if !self.config.sniper_auto_entry_enabled {
            return SniperResult {
                action: "skipped".into(),
                position_id: None,
                reason: "sniper_auto_entry_disabled".into(),
                tx_hash: None,
            };
        }

        // TVL check
        if event.tvl_usd < self.config.sniper_min_tvl {
            return SniperResult {
                action: "skipped".into(),
                position_id: None,
                reason: format!(
                    "tvl ${:.0} below minimum ${:.0}",
                    event.tvl_usd, self.config.sniper_min_tvl
                ),
                tx_hash: None,
            };
        }

        // Risk assessment
        let protocol_data = self.protocol_fetcher
            .fetch_protocol(&event.protocol)
            .await
            .unwrap_or_default();
        let risk_ctx = RiskContext {
            opportunity: crate::types::opportunity::Opportunity {
                protocol: event.protocol.clone(),
                pool: event.pool_address.clone(),
                chain: event.chain.clone(),
                tvl_usd: event.tvl_usd as u64,
                ..Default::default()
            },
            protocol_data,
        };
        let risk_decision = match self.risk.assess(&risk_ctx).await {
            Ok(d) => d,
            Err(e) => {
                return SniperResult {
                    action: "error".into(),
                    position_id: None,
                    reason: format!("risk_error: {e}"),
                    tx_hash: None,
                };
            }
        };
        if risk_decision.is_blocked() {
            return SniperResult {
                action: "blocked".into(),
                position_id: None,
                reason: "risk_gate_blocked".into(),
                tx_hash: None,
            };
        }

        // Sniper agent LLM evaluation
        let sniper_ctx = SniperContext {
            pool_address: event.pool_address.clone(),
            token0: event.token0.clone(),
            token1: event.token1.clone(),
            tvl_usd: event.tvl_usd,
            protocol: event.protocol.clone(),
            chain: event.chain.clone(),
            min_tvl_threshold: self.config.sniper_min_tvl,
        };
        let decision = match self.sniper.evaluate(&sniper_ctx).await {
            Ok(d) => d,
            Err(e) => {
                return SniperResult {
                    action: "error".into(),
                    position_id: None,
                    reason: format!("sniper_error: {e}"),
                    tx_hash: None,
                };
            }
        };

        match decision {
            SniperDecision::Pass { reasoning } => {
                tracing::info!("sniper pass on {}: {reasoning}", event.pool_address);
                SniperResult {
                    action: "pass".into(),
                    position_id: None,
                    reason: reasoning,
                    tx_hash: None,
                }
            }
            SniperDecision::Enter { confidence, max_entry_eth, reasoning } => {
                let entry_eth = max_entry_eth.min(self.config.sniper_max_entry_eth);

                if !self.config.auto_execute_enabled {
                    tracing::info!("sniper would enter {} with {:.4} ETH (auto_execute disabled)", event.pool_address, entry_eth);
                    return SniperResult {
                        action: "would_enter".into(),
                        position_id: None,
                        reason: "auto_execute_disabled".into(),
                        tx_hash: None,
                    };
                }

                // Get quote
                let chain_id = self.chain_registry
                    .chain_id(&event.chain.to_lowercase())
                    .unwrap_or(8453);
                let amount_wei = format!("{:.0}", entry_eth * 1e18);
                let _quote = self.quote_fetcher
                    .fetch_quote("WETH", &event.token1, &amount_wei, chain_id)
                    .await
                    .unwrap_or_default();

                // Execute swap via Keymaster
                let exec_result = match self.executor
                    .execute(wallet_id, "WETH", &event.token1, &amount_wei, &event.chain)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        return SniperResult {
                            action: "error".into(),
                            position_id: None,
                            reason: format!("executor_error: {e}"),
                            tx_hash: None,
                        };
                    }
                };

                if exec_result.status != ExecutorStatus::Success {
                    return SniperResult {
                        action: "error".into(),
                        position_id: None,
                        reason: exec_result.error.unwrap_or_else(|| "executor_failed".into()),
                        tx_hash: exec_result.tx_hash,
                    };
                }

                // Store entry in Redis
                let position_id = Uuid::new_v4().to_string();
                let entry = SniperEntry {
                    position_id: position_id.clone(),
                    pool_address: event.pool_address,
                    token_in: "WETH".into(),
                    token_out: event.token1,
                    amount_eth: entry_eth,
                    tx_hash: exec_result.tx_hash.clone(),
                    chain: event.chain,
                    protocol: event.protocol,
                    confidence,
                    status: "active".into(),
                    timestamp: chrono::Utc::now().timestamp(),
                };
                self.persist_entry(&entry).await;

                tracing::info!(
                    "sniper ENTERED position {} — {:.4} ETH, confidence={:.2}, tx={:?}",
                    position_id, entry_eth, confidence, exec_result.tx_hash
                );

                SniperResult {
                    action: "entered".into(),
                    position_id: Some(position_id),
                    reason: reasoning,
                    tx_hash: exec_result.tx_hash,
                }
            }
        }
    }

    pub async fn exit_position(&self, position_id: &str, wallet_id: Uuid) -> SniperResult {
        let entry = match self.load_entry(position_id).await {
            Some(e) => e,
            None => {
                return SniperResult {
                    action: "error".into(),
                    position_id: Some(position_id.to_string()),
                    reason: "position_not_found".into(),
                    tx_hash: None,
                };
            }
        };

        let amount_wei = format!("{:.0}", entry.amount_eth * 1e18);
        let exec_result = match self.executor
            .execute(wallet_id, &entry.token_out, "WETH", &amount_wei, &entry.chain)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return SniperResult {
                    action: "error".into(),
                    position_id: Some(position_id.to_string()),
                    reason: format!("exit_error: {e}"),
                    tx_hash: None,
                };
            }
        };

        SniperResult {
            action: if exec_result.status == ExecutorStatus::Success { "exited" } else { "exit_failed" }.into(),
            position_id: Some(position_id.to_string()),
            reason: exec_result.error.unwrap_or_else(|| "exit_complete".into()),
            tx_hash: exec_result.tx_hash,
        }
    }

    // ── Redis persistence ────────────────────────────────────────

    async fn persist_entry(&self, entry: &SniperEntry) {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            if let Ok(json) = serde_json::to_string(entry) {
                let _: Result<(), _> = conn.hset::<_, _, _, ()>(
                    "vespra:sniper_entries",
                    &entry.position_id,
                    &json,
                ).await;
                let _: Result<(), _> = conn.lpush::<_, _, ()>("vespra:sniper_history", &json).await;
                let _: Result<(), _> = conn.ltrim::<_, ()>("vespra:sniper_history", 0, 99).await;
            }
        }
    }

    async fn load_entry(&self, position_id: &str) -> Option<SniperEntry> {
        let mut conn = redis::Client::get_multiplexed_async_connection(self.redis.as_ref())
            .await
            .ok()?;
        let raw: Option<String> = conn.hget("vespra:sniper_entries", position_id).await.ok().flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    pub async fn active_positions(&self) -> Vec<SniperEntry> {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let all: HashMap<String, String> = conn
                .hgetall("vespra:sniper_entries")
                .await
                .unwrap_or_default();
            all.values()
                .filter_map(|s| serde_json::from_str::<SniperEntry>(s).ok())
                .filter(|e| e.status == "active")
                .collect()
        } else {
            vec![]
        }
    }
}
