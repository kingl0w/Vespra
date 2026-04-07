use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex};
use uuid::Uuid;

use crate::agents::executor::ExecutorAgent;
use crate::agents::risk::{RiskAgent, RiskContext};
use crate::agents::yield_agent::{CurrentPosition, YieldAgent, YieldCandidate, YieldContext};
use crate::config::GatewayConfig;
use crate::data::pool::PoolFetcher;
use crate::data::protocol::ProtocolFetcher;
use crate::types::decisions::YieldDecision;

// ─── Result types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YieldCycleResult {
    pub status: YieldCycleStatus,
    pub capital_eth: f64,
    pub gain_pct: Option<f64>,
    pub reason: Option<String>,
    pub cycle: u32,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum YieldCycleStatus {
    Rotated,
    Hold,
    Error,
}

impl YieldCycleResult {
    fn hold(cycle: u32, reason: &str, capital_eth: f64) -> Self {
        Self {
            status: YieldCycleStatus::Hold,
            capital_eth,
            gain_pct: None,
            reason: Some(reason.to_string()),
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    fn error(cycle: u32, reason: &str, capital_eth: f64) -> Self {
        Self {
            status: YieldCycleStatus::Error,
            capital_eth,
            gain_pct: None,
            reason: Some(reason.to_string()),
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    fn rotated(cycle: u32, capital_eth: f64, gain_pct: f64) -> Self {
        Self {
            status: YieldCycleStatus::Rotated,
            capital_eth,
            gain_pct: Some(gain_pct),
            reason: None,
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

// ─── Position state (Redis-persisted) ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YieldPosition {
    pub protocol: String,
    pub pool_id: String,
    pub apy_pct: f64,
    pub amount_eth: f64,
    pub chain: String,
    pub entered_at: i64,
}

// ─── Orchestrator ────────────────────────────────────────────────

pub struct YieldOrchestrator {
    pool_fetcher: Arc<PoolFetcher>,
    protocol_fetcher: Arc<ProtocolFetcher>,
    risk: Arc<RiskAgent>,
    yield_agent: Arc<YieldAgent>,
    executor: Arc<ExecutorAgent>,
    config: Arc<GatewayConfig>,
    redis: Arc<redis::Client>,
    kill_flag: Arc<AtomicBool>,
    active_loops: Arc<Mutex<HashMap<Uuid, (watch::Sender<bool>, tokio::task::JoinHandle<()>)>>>,
}

impl YieldOrchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool_fetcher: Arc<PoolFetcher>,
        protocol_fetcher: Arc<ProtocolFetcher>,
        risk: Arc<RiskAgent>,
        yield_agent: Arc<YieldAgent>,
        executor: Arc<ExecutorAgent>,
        config: Arc<GatewayConfig>,
        redis: Arc<redis::Client>,
        kill_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            pool_fetcher,
            protocol_fetcher,
            risk,
            yield_agent,
            executor,
            config,
            redis,
            kill_flag,
            active_loops: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn is_killed(&self) -> bool {
        self.kill_flag.load(Ordering::SeqCst)
    }

    // ═════════════════════════════════════════════════════════════
    // run_cycle
    // ═════════════════════════════════════════════════════════════

    pub async fn run_cycle(
        &self,
        wallet_id: Uuid,
        cycle_num: u32,
        capital_eth: f64,
        chain: &str,
    ) -> YieldCycleResult {
        if self.is_killed() {
            return YieldCycleResult::hold(cycle_num, "kill_switch_active", capital_eth);
        }

        // 1. Fetch current position from Redis
        let current_position = self.load_position(wallet_id).await;

        // 2. Fetch top pools for this chain
        let pools = match self.pool_fetcher.fetch(&[chain.to_string()]).await {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return YieldCycleResult::hold(cycle_num, "no_pools_available", capital_eth),
            Err(e) => {
                tracing::warn!("[yield cycle {cycle_num}] pool fetch failed: {e}");
                return YieldCycleResult::hold(cycle_num, "pool_fetch_error", capital_eth);
            }
        };

        // 3. Build top 5 candidates sorted by APY
        let mut candidates: Vec<YieldCandidate> = pools
            .iter()
            .filter(|p| p.apy > 0.0)
            .map(|p| YieldCandidate {
                protocol: p.protocol.clone(),
                pool_id: p.pool.clone(),
                apy_pct: p.apy,
                chain: p.chain.clone(),
                tvl_usd: p.tvl_usd,
                momentum_score: p.momentum_score,
            })
            .collect();
        candidates.sort_by(|a, b| b.apy_pct.partial_cmp(&a.apy_pct).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(5);

        if candidates.is_empty() {
            return YieldCycleResult::hold(cycle_num, "no_yield_candidates", capital_eth);
        }

        // 4. Risk check on the top candidate
        let best = &candidates[0];
        let protocol_data = self.protocol_fetcher
            .fetch_protocol(&best.protocol)
            .await
            .unwrap_or_default();
        let risk_ctx = RiskContext {
            chain: best.chain.clone(),
            opportunity: crate::types::opportunity::Opportunity {
                protocol: best.protocol.clone(),
                pool: best.pool_id.clone(),
                chain: best.chain.clone(),
                apy: best.apy_pct,
                tvl_usd: best.tvl_usd,
                momentum_score: best.momentum_score,
                ..Default::default()
            },
            protocol_data,
        };
        let risk_decision = match self.risk.assess(&risk_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("[yield cycle {cycle_num}] risk failed: {e}");
                return YieldCycleResult::hold(cycle_num, "risk_error", capital_eth);
            }
        };

        if risk_decision.is_blocked() {
            tracing::info!("[yield cycle {cycle_num}] risk gate blocked: {:?}", risk_decision);
            return YieldCycleResult::hold(cycle_num, "risk_gate_blocked", capital_eth);
        }

        // 5. Run yield agent
        let current = current_position.as_ref().map(|p| CurrentPosition {
            protocol: p.protocol.clone(),
            apy_pct: p.apy_pct,
            amount_eth: p.amount_eth,
        });
        let yield_ctx = YieldContext {
            current_position: current,
            candidates,
            threshold_pct: self.config.yield_auto_rotate_threshold_pct,
        };
        let yield_decision = match self.yield_agent.evaluate(&yield_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("[yield cycle {cycle_num}] yield agent failed: {e}");
                return YieldCycleResult::hold(cycle_num, "yield_agent_error", capital_eth);
            }
        };

        match yield_decision {
            YieldDecision::Hold { reasoning } => {
                tracing::info!("[yield cycle {cycle_num}] hold: {reasoning}");
                YieldCycleResult::hold(cycle_num, &reasoning, capital_eth)
            }
            YieldDecision::Rebalance { target_protocol, target_pool_id, expected_gain_pct, reasoning } => {
                if expected_gain_pct < self.config.yield_auto_rotate_threshold_pct {
                    return YieldCycleResult::hold(
                        cycle_num,
                        &format!("gain {:.2}% below threshold {:.2}%", expected_gain_pct, self.config.yield_auto_rotate_threshold_pct),
                        capital_eth,
                    );
                }

                if !self.config.auto_execute_enabled {
                    tracing::info!("[yield cycle {cycle_num}] auto_execute disabled — would rotate to {target_protocol}");
                    return YieldCycleResult::hold(cycle_num, "auto_execute_disabled", capital_eth);
                }

                tracing::info!(
                    "[yield cycle {cycle_num}] rotating: {} → {} gain={:.2}% reason={}",
                    current_position.as_ref().map(|p| p.protocol.as_str()).unwrap_or("none"),
                    target_protocol,
                    expected_gain_pct,
                    reasoning
                );

                // Withdraw from current position (if any)
                if let Some(ref pos) = current_position {
                    let amount_wei = format!("{:.0}", pos.amount_eth * 1e18);
                    if let Err(e) = self.executor
                        .execute(wallet_id, &pos.pool_id, "WETH", &amount_wei, chain)
                        .await
                    {
                        tracing::error!("[yield cycle {cycle_num}] withdraw failed: {e}");
                        return YieldCycleResult::error(cycle_num, &format!("withdraw_error: {e}"), capital_eth);
                    }
                }

                // Deposit to new position
                let amount_wei = format!("{:.0}", capital_eth * 1e18);
                match self.executor
                    .execute(wallet_id, "WETH", &target_pool_id, &amount_wei, chain)
                    .await
                {
                    Ok(result) => {
                        if result.status != crate::types::decisions::ExecutorStatus::Success {
                            tracing::error!("[yield cycle {cycle_num}] deposit failed, sweeping to Safe");
                            let _ = self.executor
                                .execute(wallet_id, "WETH", "SAFE", &amount_wei, chain)
                                .await;
                            return YieldCycleResult::error(cycle_num, "deposit_failed_swept_to_safe", capital_eth);
                        }
                    }
                    Err(e) => {
                        tracing::error!("[yield cycle {cycle_num}] deposit failed: {e}, sweeping to Safe");
                        let _ = self.executor
                            .execute(wallet_id, "WETH", "SAFE", &amount_wei, chain)
                            .await;
                        return YieldCycleResult::error(cycle_num, &format!("deposit_error: {e}"), capital_eth);
                    }
                }

                // Update position in Redis
                let new_pos = YieldPosition {
                    protocol: target_protocol,
                    pool_id: target_pool_id,
                    apy_pct: expected_gain_pct,
                    amount_eth: capital_eth,
                    chain: chain.to_string(),
                    entered_at: chrono::Utc::now().timestamp(),
                };
                self.save_position(wallet_id, &new_pos).await;

                YieldCycleResult::rotated(cycle_num, capital_eth, expected_gain_pct)
            }
        }
    }

    // ── Redis persistence ────────────────────────────────────────

    async fn load_position(&self, wallet_id: Uuid) -> Option<YieldPosition> {
        let mut conn = redis::Client::get_multiplexed_async_connection(self.redis.as_ref())
            .await
            .ok()?;
        let key = format!("vespra:yield_position:{wallet_id}");
        let raw: Option<String> = conn.get(&key).await.ok().flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    async fn save_position(&self, wallet_id: Uuid, pos: &YieldPosition) {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let key = format!("vespra:yield_position:{wallet_id}");
            if let Ok(json) = serde_json::to_string(pos) {
                let _: Result<(), _> = conn.set::<_, _, ()>(&key, &json).await;
            }
        }
    }

    pub async fn persist_to_history(&self, wallet_id: Uuid, result: &YieldCycleResult) {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let status_str = match result.status {
                YieldCycleStatus::Rotated => "rotated",
                YieldCycleStatus::Hold => "hold",
                YieldCycleStatus::Error => "error",
            };
            let entry = serde_json::json!({
                "wallet_id": wallet_id.to_string(),
                "cycle": result.cycle,
                "status": status_str,
                "reason": result.reason,
                "capital_eth": result.capital_eth,
                "gain_pct": result.gain_pct,
                "timestamp": result.timestamp,
            });
            if let Ok(json) = serde_json::to_string(&entry) {
                let wallet_key = format!("vespra:yield_rotations:{wallet_id}");
                let _: Result<(), _> = conn.lpush::<_, _, ()>(&wallet_key, &json).await;
                let _: Result<(), _> = conn.ltrim::<_, ()>(&wallet_key, 0, 99).await;
                let _: Result<(), _> = conn.lpush::<_, _, ()>("vespra:yield_rotations", &json).await;
                let _: Result<(), _> = conn.ltrim::<_, ()>("vespra:yield_rotations", 0, 99).await;
            }
        }
    }

    async fn persist_loop_state(&self, wallet_id: Uuid, cycle: u32, capital_eth: f64, status: &str, running: bool) {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let state = serde_json::json!({
                "wallet_id": wallet_id.to_string(),
                "cycle": cycle,
                "capital_eth": capital_eth,
                "status": status,
                "running": running,
                "timestamp": chrono::Utc::now().timestamp(),
            });
            if let Ok(json) = serde_json::to_string(&state) {
                let key = format!("vespra:yield_state:{wallet_id}");
                let _: Result<(), _> = conn.set::<_, _, ()>(&key, &json).await;
            }
        }
    }

    // ── Loop management ──────────────────────────────────────────

    pub async fn start_loop(
        self: &Arc<Self>,
        wallet_id: Uuid,
        capital_eth: f64,
        chain: String,
    ) -> Result<()> {
        let mut loops = self.active_loops.lock().await;
        if loops.contains_key(&wallet_id) {
            anyhow::bail!("yield loop already running for wallet {wallet_id}");
        }

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let orch = Arc::clone(self);
        let interval_secs = self.config.yield_cycle_interval_secs;

        let handle = tokio::spawn(async move {
            run_yield_loop(orch, wallet_id, capital_eth, chain, cancel_rx, interval_secs).await;
        });

        loops.insert(wallet_id, (cancel_tx, handle));
        tracing::info!("yield loop started for wallet {wallet_id}");
        Ok(())
    }

    pub async fn stop_loop(&self, wallet_id: Uuid) -> Result<()> {
        let mut loops = self.active_loops.lock().await;
        if let Some((cancel_tx, handle)) = loops.remove(&wallet_id) {
            let _ = cancel_tx.send(true);
            drop(handle);
            tracing::info!("yield loop stop requested for wallet {wallet_id}");
            Ok(())
        } else {
            anyhow::bail!("no active yield loop for wallet {wallet_id}");
        }
    }

    pub async fn active_wallets(&self) -> Vec<Uuid> {
        let loops = self.active_loops.lock().await;
        loops.keys().copied().collect()
    }
}

// ─── Background loop task ────────────────────────────────────────

async fn run_yield_loop(
    orch: Arc<YieldOrchestrator>,
    wallet_id: Uuid,
    initial_capital: f64,
    chain: String,
    mut cancel_rx: watch::Receiver<bool>,
    interval_secs: u64,
) {
    let capital = initial_capital;
    let mut cycle: u32 = 0;

    orch.persist_loop_state(wallet_id, 0, capital, "started", true).await;

    loop {
        if orch.is_killed() {
            tracing::warn!("kill switch active — halting yield loop for wallet {wallet_id}");
            break;
        }
        if *cancel_rx.borrow() {
            tracing::info!("yield loop cancelled for wallet {wallet_id}");
            break;
        }

        cycle += 1;
        let result = orch.run_cycle(wallet_id, cycle, capital, &chain).await;

        tracing::info!(
            wallet = %wallet_id,
            cycle,
            status = ?result.status,
            capital = result.capital_eth,
            gain = ?result.gain_pct,
            reason = ?result.reason,
            "yield cycle complete"
        );

        let status_str = match result.status {
            YieldCycleStatus::Rotated => "rotated",
            YieldCycleStatus::Hold => "hold",
            YieldCycleStatus::Error => "error",
        };
        orch.persist_loop_state(wallet_id, cycle, capital, status_str, true).await;
        orch.persist_to_history(wallet_id, &result).await;

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval_secs)) => {}
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::info!("yield loop cancelled during sleep for wallet {wallet_id}");
                    break;
                }
            }
        }
    }

    orch.persist_loop_state(wallet_id, cycle, capital, "stopped", false).await;
    let mut loops = orch.active_loops.lock().await;
    loops.remove(&wallet_id);
    tracing::info!(wallet = %wallet_id, cycles = cycle, "yield loop ended");
}
