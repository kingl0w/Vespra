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
use crate::agents::scout::{ScoutAgent, ScoutContext};
use crate::agents::sentinel::{SentinelAgent, SentinelContext};
use crate::agents::trader::{TraderAgent, TraderContext};
use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;
use crate::data::pool::PoolFetcher;
use crate::data::price::PriceOracle;
use crate::data::protocol::ProtocolFetcher;
use crate::data::quote::QuoteFetcher;
use crate::data::wallet::WalletFetcher;
use crate::types::decisions::{ExecutorStatus, ScoutDecision, SentinelDecision, TraderDecision};
use crate::types::wallet::PriceData;

// ─── Result types ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CycleResult {
    pub status: CycleStatus,
    pub capital_eth: f64,
    pub gain_pct: Option<f64>,
    pub tx_hash: Option<String>,
    pub reason: Option<String>,
    pub cycle: u32,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CycleStatus {
    Executed,
    Hold,
    Exit,
    Error,
}

impl CycleResult {
    pub fn hold(cycle: u32, reason: &str) -> Self {
        Self {
            status: CycleStatus::Hold,
            capital_eth: 0.0, // set by caller
            gain_pct: None,
            tx_hash: None,
            reason: Some(reason.to_string()),
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    pub fn exit(cycle: u32, reason: &str) -> Self {
        Self {
            status: CycleStatus::Exit,
            capital_eth: 0.0,
            gain_pct: None,
            tx_hash: None,
            reason: Some(reason.to_string()),
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    pub fn error(cycle: u32, reason: &str) -> Self {
        Self {
            status: CycleStatus::Error,
            capital_eth: 0.0,
            gain_pct: None,
            tx_hash: None,
            reason: Some(reason.to_string()),
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    pub fn executed(cycle: u32, capital_eth: f64, gain_pct: f64, tx_hash: Option<String>) -> Self {
        Self {
            status: CycleStatus::Executed,
            capital_eth,
            gain_pct: Some(gain_pct),
            tx_hash,
            reason: None,
            cycle,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    /// Fill in capital_eth for non-executed results
    fn with_capital(mut self, capital_eth: f64) -> Self {
        if self.status != CycleStatus::Executed {
            self.capital_eth = capital_eth;
        }
        self
    }
}

// ─── Orchestrator ────────────────────────────────────────────────

pub struct TradeUpOrchestrator {
    pub pool_fetcher: Arc<PoolFetcher>,
    pub protocol_fetcher: Arc<ProtocolFetcher>,
    pub price_oracle: Arc<dyn PriceOracle>,
    pub wallet_fetcher: Arc<WalletFetcher>,
    pub quote_fetcher: Arc<QuoteFetcher>,
    pub scout: Arc<ScoutAgent>,
    pub risk: Arc<RiskAgent>,
    pub trader: Arc<TraderAgent>,
    pub sentinel: Arc<SentinelAgent>,
    pub executor: Arc<ExecutorAgent>,
    pub config: Arc<GatewayConfig>,
    pub chain_registry: Arc<ChainRegistry>,
    pub redis: Arc<redis::Client>,
    pub kill_flag: Arc<AtomicBool>,
    active_loops: Arc<Mutex<HashMap<Uuid, (watch::Sender<bool>, tokio::task::JoinHandle<()>)>>>,
}

impl TradeUpOrchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool_fetcher: Arc<PoolFetcher>,
        protocol_fetcher: Arc<ProtocolFetcher>,
        price_oracle: Arc<dyn PriceOracle>,
        wallet_fetcher: Arc<WalletFetcher>,
        quote_fetcher: Arc<QuoteFetcher>,
        scout: Arc<ScoutAgent>,
        risk: Arc<RiskAgent>,
        trader: Arc<TraderAgent>,
        sentinel: Arc<SentinelAgent>,
        executor: Arc<ExecutorAgent>,
        config: Arc<GatewayConfig>,
        chain_registry: Arc<ChainRegistry>,
        redis: Arc<redis::Client>,
        kill_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            pool_fetcher,
            protocol_fetcher,
            price_oracle,
            wallet_fetcher,
            quote_fetcher,
            scout,
            risk,
            trader,
            sentinel,
            executor,
            config,
            chain_registry,
            redis,
            kill_flag,
            active_loops: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns true if the global kill switch is active.
    pub fn is_killed(&self) -> bool {
        self.kill_flag.load(Ordering::SeqCst)
    }

    // ═════════════════════════════════════════════════════════════
    // run_cycle — the core function
    // ═════════════════════════════════════════════════════════════

    pub async fn run_cycle(
        &self,
        wallet_id: Uuid,
        cycle_num: u32,
        capital_eth: f64,
        chains: &[String],
    ) -> CycleResult {
        // Kill switch check — top of cycle
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — aborting cycle for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        // ═══════════════════════════════════════
        // PHASE 1: DATA FETCH (no agents)
        // ═══════════════════════════════════════

        // 1. Fetch all pools for requested chains
        let pools = match self.pool_fetcher.fetch(chains).await {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return CycleResult::hold(cycle_num, "no_pools_available").with_capital(capital_eth),
            Err(e) => {
                tracing::warn!("[cycle {cycle_num}] pool fetch failed: {e}");
                return CycleResult::hold(cycle_num, "pool_fetch_error").with_capital(capital_eth);
            }
        };

        // 2. Pre-select top candidate by momentum_score for targeted data fetching
        let candidate = pools
            .iter()
            .max_by(|a, b| {
                a.momentum_score
                    .partial_cmp(&b.momentum_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap(); // safe: pools is non-empty

        // 3. Fetch protocol data for candidate (non-blocking failure)
        let protocol_data = self
            .protocol_fetcher
            .fetch_protocol(&candidate.protocol)
            .await
            .unwrap_or_default();

        // 4. Fetch price data (non-blocking failure)
        let _price_data: PriceData = self
            .price_oracle
            .fetch(&candidate.pool, &candidate.chain)
            .await
            .unwrap_or_default();

        // 5. Fetch wallet state (non-blocking failure)
        let wallets = self
            .wallet_fetcher
            .fetch_wallets(&candidate.chain)
            .await
            .unwrap_or_default();

        // ═══════════════════════════════════════
        // PHASE 2: AGENT DECISIONS (no data fetching)
        // ═══════════════════════════════════════

        // Kill switch check — before scout
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before scout for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        // 6. Scout decision
        let scout_ctx = ScoutContext {
            wallet_id,
            mode: "momentum".to_string(),
            pools: pools.clone(),
            chains: chains.to_vec(),
        };
        let best = match self.scout.analyze(&scout_ctx).await {
            Ok(ScoutDecision::Opportunities(opps)) => {
                match opps
                    .into_iter()
                    .max_by(|a, b| {
                        a.momentum_score
                            .partial_cmp(&b.momentum_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                    Some(o) if o.momentum_score >= 0.6 => o,
                    Some(o) => {
                        return CycleResult::hold(
                            cycle_num,
                            &format!("momentum_below_threshold: {:.2}", o.momentum_score),
                        )
                        .with_capital(capital_eth);
                    }
                    None => {
                        return CycleResult::hold(cycle_num, "scout_returned_empty")
                            .with_capital(capital_eth);
                    }
                }
            }
            Ok(ScoutDecision::NoOpportunities { reason }) => {
                return CycleResult::hold(cycle_num, &reason).with_capital(capital_eth);
            }
            Err(e) => {
                tracing::error!("[cycle {cycle_num}] scout failed: {e}");
                return CycleResult::hold(cycle_num, "scout_error").with_capital(capital_eth);
            }
        };

        tracing::info!(
            "[cycle {cycle_num}] scout selected: {} {} apy={:.1}% momentum={:.2}",
            best.protocol,
            best.pool,
            best.apy,
            best.momentum_score
        );

        // Kill switch check — before risk
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before risk for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        // 7. Risk decision
        let risk_ctx = RiskContext {
            opportunity: best.clone(),
            protocol_data: protocol_data.clone(),
        };
        let risk_decision = match self.risk.assess(&risk_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("[cycle {cycle_num}] risk failed: {e}");
                return CycleResult::hold(cycle_num, "risk_error").with_capital(capital_eth);
            }
        };

        if risk_decision.is_blocked() {
            tracing::info!("[cycle {cycle_num}] risk gate blocked: {:?}", risk_decision);
            return CycleResult::hold(cycle_num, "risk_gate_blocked").with_capital(capital_eth);
        }
        tracing::info!("[cycle {cycle_num}] risk gate passed: {:?}", risk_decision);

        // Kill switch check — before sentinel
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before sentinel for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        // 8. Sentinel decision
        let sentinel_ctx = SentinelContext {
            wallets: wallets.clone(),
            stop_loss_pct: self.config.trade_up_stop_loss_pct,
        };
        let sentinel_decision = match self.sentinel.check(&sentinel_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[cycle {cycle_num}] sentinel failed (non-fatal): {e}");
                SentinelDecision::Healthy
            }
        };

        if sentinel_decision.is_stop_loss() {
            return CycleResult::exit(cycle_num, "stop_loss_triggered").with_capital(capital_eth);
        }

        // 9. Get swap quote (1inch real or simulated fallback)
        let chain_id = self
            .chain_registry
            .chain_id(&best.chain.to_lowercase())
            .unwrap_or(8453);
        let amount_wei = format!("{:.0}", capital_eth * 1e18);
        let quote = self
            .quote_fetcher
            .fetch_quote("WETH", &best.pool, &amount_wei, chain_id)
            .await
            .unwrap_or_default();

        // Kill switch check — before trader
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before trader for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        // 10. Trader decision
        let trader_ctx = TraderContext {
            opportunity: best.clone(),
            quote: quote.clone(),
            capital_eth,
            risk_score: risk_decision.score().clone(),
            min_gain_pct: self.config.trade_up_min_gain_pct,
            max_eth: self.config.trade_up_max_eth,
        };
        let trader_decision = match self.trader.evaluate(&trader_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("[cycle {cycle_num}] trader failed: {e}");
                return CycleResult::hold(cycle_num, "trader_error").with_capital(capital_eth);
            }
        };

        // ═══════════════════════════════════════
        // PHASE 3: BUSINESS LOGIC (orchestrator owns this)
        // ═══════════════════════════════════════

        match trader_decision {
            TraderDecision::Hold { reason } => {
                CycleResult::hold(cycle_num, &reason).with_capital(capital_eth)
            }
            TraderDecision::Exit { reason } => {
                CycleResult::exit(cycle_num, &reason).with_capital(capital_eth)
            }
            TraderDecision::Swap {
                token_in,
                token_out,
                amount_in_wei,
                expected_gain_pct,
                ..
            } => {
                // Yield bypass — orchestrator decides this, NOT the agent
                let effective_gain = if expected_gain_pct == 0.0 && best.is_yield_position() {
                    best.expected_yield_gain_pct(self.config.trade_up_cycle_interval_secs)
                } else {
                    expected_gain_pct
                };

                // Gain check — yield positions bypass if APY >= 50%
                if !best.is_yield_position()
                    && effective_gain < self.config.trade_up_min_gain_pct
                {
                    return CycleResult::hold(
                        cycle_num,
                        &format!(
                            "gain {:.4}% below min {:.2}%",
                            effective_gain, self.config.trade_up_min_gain_pct
                        ),
                    )
                    .with_capital(capital_eth);
                }

                tracing::info!(
                    "[cycle {cycle_num}] executing swap: {} → {} gain={:.4}% yield_bypass={}",
                    token_in,
                    token_out,
                    effective_gain,
                    best.is_yield_position()
                );

                // Auto-execute gate
                if !self.config.auto_execute_enabled {
                    tracing::info!(
                        "[cycle {cycle_num}] auto_execute disabled — queuing for approval"
                    );
                    return CycleResult::hold(cycle_num, "auto_execute_disabled")
                        .with_capital(capital_eth);
                }

                // Execute via Keymaster
                let exec_result = match self
                    .executor
                    .execute(wallet_id, &token_in, &token_out, &amount_in_wei, &best.chain)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        return CycleResult::error(
                            cycle_num,
                            &format!("executor_error: {e}"),
                        )
                        .with_capital(capital_eth);
                    }
                };

                if exec_result.status != ExecutorStatus::Success {
                    return CycleResult::error(
                        cycle_num,
                        &exec_result.error.unwrap_or_else(|| "executor_failed".into()),
                    )
                    .with_capital(capital_eth);
                }

                // Compound capital
                let new_capital = capital_eth * (1.0 + effective_gain / 100.0);

                // Persist to Redis history
                let _ = self
                    .persist_cycle_result(
                        wallet_id,
                        cycle_num,
                        capital_eth,
                        new_capital,
                        effective_gain,
                        &exec_result.tx_hash,
                    )
                    .await;

                tracing::info!(
                    "[cycle {cycle_num}] EXECUTED — capital: {:.6} → {:.6} ETH tx={:?}",
                    capital_eth,
                    new_capital,
                    exec_result.tx_hash
                );

                CycleResult::executed(cycle_num, new_capital, effective_gain, exec_result.tx_hash)
            }
        }
    }

    // ── Redis persistence ────────────────────────────────────────

    async fn persist_cycle_result(
        &self,
        wallet_id: Uuid,
        cycle_num: u32,
        old_capital: f64,
        new_capital: f64,
        gain_pct: f64,
        tx_hash: &Option<String>,
    ) -> Result<()> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;

        let entry = serde_json::json!({
            "wallet_id": wallet_id.to_string(),
            "cycle": cycle_num,
            "old_capital_eth": old_capital,
            "new_capital_eth": new_capital,
            "gain_pct": gain_pct,
            "tx_hash": tx_hash,
            "timestamp": chrono::Utc::now().timestamp(),
        });

        let json = serde_json::to_string(&entry)?;
        conn.lpush::<_, _, ()>("vespra:trade_up_history", &json).await?;
        conn.ltrim::<_, ()>("vespra:trade_up_history", 0, 99).await?;

        Ok(())
    }

    /// Persist every cycle result to the per-wallet and global history lists.
    pub async fn persist_cycle_to_history(&self, wallet_id: Uuid, result: &CycleResult) -> Result<()> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;

        let status_str = match result.status {
            CycleStatus::Executed => "executed",
            CycleStatus::Hold => "hold",
            CycleStatus::Exit => "exit",
            CycleStatus::Error => "error",
        };

        let entry = serde_json::json!({
            "wallet_id": wallet_id.to_string(),
            "cycle": result.cycle,
            "status": status_str,
            "reason": result.reason,
            "capital_eth": result.capital_eth,
            "gain_pct": result.gain_pct,
            "tx_hash": result.tx_hash,
            "timestamp": result.timestamp,
        });

        let json = serde_json::to_string(&entry)?;

        // Per-wallet history
        let wallet_key = format!("vespra:trade_up_history:{wallet_id}");
        conn.lpush::<_, _, ()>(&wallet_key, &json).await?;
        conn.ltrim::<_, ()>(&wallet_key, 0, 99).await?;

        // Global history
        conn.lpush::<_, _, ()>("vespra:trade_up_history", &json).await?;
        conn.ltrim::<_, ()>("vespra:trade_up_history", 0, 99).await?;

        Ok(())
    }

    async fn persist_loop_state(
        &self,
        wallet_id: Uuid,
        cycle: u32,
        capital_eth: f64,
        status: &str,
        running: bool,
    ) {
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let state = serde_json::json!({
                "wallet_id": wallet_id.to_string(),
                "cycle": cycle,
                "capital_eth": capital_eth,
                "status": status,
                "running": running,
                "timestamp": chrono::Utc::now().timestamp(),
            });
            if let Ok(json) = serde_json::to_string(&state) {
                let key = format!("vespra:trade_up_state:{wallet_id}");
                let _: Result<(), _> = conn.set::<_, _, ()>(&key, &json).await;
            }
        }
    }

    // ── Loop management ──────────────────────────────────────────

    pub async fn start_loop(
        self: &Arc<Self>,
        wallet_id: Uuid,
        initial_capital_eth: f64,
        chains: Vec<String>,
    ) -> Result<()> {
        let mut loops = self.active_loops.lock().await;
        if loops.contains_key(&wallet_id) {
            anyhow::bail!("trade-up loop already running for wallet {wallet_id}");
        }

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let orch = Arc::clone(self);
        let interval_secs = self.config.trade_up_cycle_interval_secs;
        let stop_loss_pct = self.config.trade_up_stop_loss_pct;

        let handle = tokio::spawn(async move {
            run_loop_task(
                orch,
                wallet_id,
                initial_capital_eth,
                chains,
                cancel_rx,
                interval_secs,
                stop_loss_pct,
            )
            .await;
        });

        loops.insert(wallet_id, (cancel_tx, handle));
        tracing::info!("trade-up loop started for wallet {wallet_id}");
        Ok(())
    }

    pub async fn stop_loop(&self, wallet_id: Uuid) -> Result<()> {
        let mut loops = self.active_loops.lock().await;
        if let Some((cancel_tx, handle)) = loops.remove(&wallet_id) {
            let _ = cancel_tx.send(true);
            drop(handle);
            tracing::info!("trade-up loop stop requested for wallet {wallet_id}");
            Ok(())
        } else {
            anyhow::bail!("no active trade-up loop for wallet {wallet_id}");
        }
    }

    pub async fn active_wallets(&self) -> Vec<Uuid> {
        let loops = self.active_loops.lock().await;
        loops.keys().copied().collect()
    }
}

// ─── Background loop task ────────────────────────────────────────

async fn run_loop_task(
    orch: Arc<TradeUpOrchestrator>,
    wallet_id: Uuid,
    initial_capital: f64,
    chains: Vec<String>,
    mut cancel_rx: watch::Receiver<bool>,
    interval_secs: u64,
    stop_loss_pct: f64,
) {
    let mut capital = initial_capital;
    let mut cycle: u32 = 0;
    let mut peak_capital = initial_capital;

    // Persist initial state
    orch.persist_loop_state(wallet_id, 0, capital, "started", true)
        .await;

    loop {
        // Check kill switch
        if orch.is_killed() {
            tracing::warn!("kill switch active — halting loop for wallet {wallet_id}");
            break;
        }

        // Check cancellation
        if *cancel_rx.borrow() {
            tracing::info!("trade-up loop cancelled for wallet {wallet_id}");
            break;
        }

        cycle += 1;
        let result = orch
            .run_cycle(wallet_id, cycle, capital, &chains)
            .await;

        tracing::info!(
            wallet = %wallet_id,
            cycle,
            status = ?result.status,
            capital = result.capital_eth,
            gain = ?result.gain_pct,
            reason = ?result.reason,
            "trade-up cycle complete"
        );

        capital = result.capital_eth;
        if capital > peak_capital {
            peak_capital = capital;
        }

        // Persist state after each cycle
        let status_str = match result.status {
            CycleStatus::Executed => "executed",
            CycleStatus::Hold => "hold",
            CycleStatus::Exit => "exit",
            CycleStatus::Error => "error",
        };
        orch.persist_loop_state(wallet_id, cycle, capital, status_str, true)
            .await;

        // Persist cycle to history list (every cycle, not just executed swaps)
        let _ = orch.persist_cycle_to_history(wallet_id, &result).await;

        // Stop-loss: check drawdown from peak
        let drawdown_pct = if peak_capital > 0.0 {
            ((peak_capital - capital) / peak_capital) * 100.0
        } else {
            0.0
        };
        if drawdown_pct >= stop_loss_pct {
            tracing::warn!(
                wallet = %wallet_id,
                drawdown_pct,
                peak_capital,
                capital,
                "stop-loss triggered in loop"
            );
            break;
        }

        // Exit status ends the loop
        if result.status == CycleStatus::Exit {
            break;
        }

        // Wait for next cycle or cancellation
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval_secs)) => {}
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    tracing::info!("trade-up loop cancelled during sleep for wallet {wallet_id}");
                    break;
                }
            }
        }
    }

    // Persist final state
    orch.persist_loop_state(wallet_id, cycle, capital, "stopped", false)
        .await;

    // Clean up from active loops
    let mut loops = orch.active_loops.lock().await;
    loops.remove(&wallet_id);
    tracing::info!(
        wallet = %wallet_id,
        cycles = cycle,
        initial_capital = initial_capital,
        final_capital = capital,
        pnl_pct = ((capital - initial_capital) / initial_capital) * 100.0,
        "trade-up loop ended"
    );
}
