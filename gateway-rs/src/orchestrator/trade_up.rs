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
use crate::types::trade_up::{
    PositionStatus, TradePosition, REDIS_ACTIVE_POSITION, REDIS_TRADE_POSITIONS,
};
use crate::types::wallet::PriceData;

///deterministic uuid from wallet+chain for dedup of position loops.
fn make_loop_key(wallet: &str, chain: &str) -> Uuid {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    wallet.hash(&mut hasher);
    chain.hash(&mut hasher);
    let h = hasher.finish();
    let bytes = h.to_le_bytes();
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes[..8].copy_from_slice(&bytes);
    uuid_bytes[8..16].copy_from_slice(&bytes);
    Uuid::from_bytes(uuid_bytes)
}

//─── position loop state machine ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum LoopPhase {
    Idle,
    Scouting,
    RiskCheck,
    Entering,
    Monitoring,
    Exiting,
    Compounding,
}

impl std::fmt::Display for LoopPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Scouting => write!(f, "scouting"),
            Self::RiskCheck => write!(f, "risk_check"),
            Self::Entering => write!(f, "entering"),
            Self::Monitoring => write!(f, "monitoring"),
            Self::Exiting => write!(f, "exiting"),
            Self::Compounding => write!(f, "compounding"),
        }
    }
}

//─── result types ────────────────────────────────────────────────

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

    ///fill in capital_eth for non-executed results
    fn with_capital(mut self, capital_eth: f64) -> Self {
        if self.status != CycleStatus::Executed {
            self.capital_eth = capital_eth;
        }
        self
    }
}

//─── orchestrator ────────────────────────────────────────────────

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

    ///returns true if the global kill switch is active.
    pub fn is_killed(&self) -> bool {
        self.kill_flag.load(Ordering::SeqCst)
    }


    pub async fn run_cycle(
        &self,
        wallet_id: Uuid,
        cycle_num: u32,
        capital_eth: f64,
        chains: &[String],
    ) -> CycleResult {
        //kill switch check — top of cycle
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — aborting cycle for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }


        //1. fetch all pools for requested chains
        let pools = match self.pool_fetcher.fetch(chains).await {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return CycleResult::hold(cycle_num, "no_pools_available").with_capital(capital_eth),
            Err(e) => {
                tracing::warn!("[cycle {cycle_num}] pool fetch failed: {e}");
                return CycleResult::hold(cycle_num, "pool_fetch_error").with_capital(capital_eth);
            }
        };

        //2. pre-select top candidate by momentum_score for targeted data fetching
        let candidate = pools
            .iter()
            .max_by(|a, b| {
                a.momentum_score
                    .partial_cmp(&b.momentum_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap(); // safe: pools is non-empty

        //3. fetch protocol data for candidate (non-blocking failure)
        let protocol_data = self
            .protocol_fetcher
            .fetch_protocol(&candidate.protocol)
            .await
            .unwrap_or_default();

        //4. fetch price data (non-blocking failure)
        let _price_data: PriceData = self
            .price_oracle
            .fetch(&candidate.pool, &candidate.chain)
            .await
            .unwrap_or_default();

        //5. fetch wallet state (non-blocking failure)
        let wallets = self
            .wallet_fetcher
            .fetch_wallets(&candidate.chain)
            .await
            .unwrap_or_default();


        //kill switch check — before scout
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before scout for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        //6. scout decision
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

        //volatility gate — reject high 24h price swings
        if best.price_change_24h_pct.abs() > self.config.volatility_gate_threshold {
            tracing::warn!(
                "[cycle {cycle_num}] volatility gate: {:.1}% 24h change exceeds {:.1}% — skipping cycle",
                best.price_change_24h_pct,
                self.config.volatility_gate_threshold
            );
            return CycleResult::hold(cycle_num, "volatility_gate").with_capital(capital_eth);
        }

        //kill switch check — before risk
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before risk for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        //7. risk decision
        let risk_ctx = RiskContext {
            chain: best.chain.clone(),
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

        //kill switch check — before sentinel
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before sentinel for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        //8. sentinel decision
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

        //9. get swap quote (1inch real or simulated fallback)
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

        //slippage guard — reject if price impact exceeds config threshold
        if !crate::guards::slippage_ok(quote.price_impact, &self.config) {
            return CycleResult::hold(cycle_num, "slippage_guard").with_capital(capital_eth);
        }

        //kill switch check — before trader
        if self.is_killed() {
            tracing::warn!("[cycle {cycle_num}] kill switch active — halting before trader for wallet {wallet_id}");
            return CycleResult::exit(cycle_num, "kill_switch_active").with_capital(capital_eth);
        }

        //10. trader decision
        let trader_ctx = TraderContext {
            chain: best.chain.clone(),
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
                //yield bypass — orchestrator decides this, not the agent
                let effective_gain = if expected_gain_pct == 0.0 && best.is_yield_position() {
                    best.expected_yield_gain_pct(self.config.trade_up_cycle_interval_secs)
                } else {
                    expected_gain_pct
                };

                //gain check — yield positions bypass if apy >= 50%
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

                //auto-execute gate
                if !self.config.auto_execute_enabled {
                    tracing::info!(
                        "[cycle {cycle_num}] auto_execute disabled — queuing for approval"
                    );
                    return CycleResult::hold(cycle_num, "auto_execute_disabled")
                        .with_capital(capital_eth);
                }

                //execute via keymaster
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

                //compound capital
                let new_capital = capital_eth * (1.0 + effective_gain / 100.0);

                //persist to redis history
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

    //── redis persistence ────────────────────────────────────────

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

    ///persist every cycle result to the per-wallet and global history lists.
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

        //per-wallet history
        let wallet_key = format!("vespra:trade_up_history:{wallet_id}");
        conn.lpush::<_, _, ()>(&wallet_key, &json).await?;
        conn.ltrim::<_, ()>(&wallet_key, 0, 99).await?;

        //global history
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

    //── loop management ──────────────────────────────────────────

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

    //── position redis helpers ───────────────────────────────────

    async fn save_position(&self, position: &TradePosition) -> Result<()> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;
        let json = serde_json::to_string(position)?;
        conn.lpush::<_, _, ()>(REDIS_TRADE_POSITIONS, &json).await?;
        conn.ltrim::<_, ()>(REDIS_TRADE_POSITIONS, 0, 199).await?;
        Ok(())
    }

    async fn set_active_position(&self, position_id: &str) -> Result<()> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;
        conn.set::<_, _, ()>(REDIS_ACTIVE_POSITION, position_id).await?;
        Ok(())
    }

    async fn clear_active_position(&self) -> Result<()> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;
        conn.del::<_, ()>(REDIS_ACTIVE_POSITION).await?;
        Ok(())
    }

    pub async fn get_active_position(&self) -> Result<Option<TradePosition>> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;
        let active_id: Option<String> = conn.get(REDIS_ACTIVE_POSITION).await?;
        let active_id = match active_id {
            Some(id) => id,
            None => return Ok(None),
        };
        let raw: Vec<String> = conn.lrange(REDIS_TRADE_POSITIONS, 0, 199).await?;
        for entry in raw {
            if let Ok(pos) = serde_json::from_str::<TradePosition>(&entry) {
                if pos.id == active_id {
                    return Ok(Some(pos));
                }
            }
        }
        Ok(None)
    }

    pub async fn get_all_positions(&self) -> Result<Vec<TradePosition>> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;
        let raw: Vec<String> = conn.lrange(REDIS_TRADE_POSITIONS, 0, 199).await?;
        let mut positions: Vec<TradePosition> = raw
            .iter()
            .filter_map(|s| serde_json::from_str(s).ok())
            .collect();
        positions.sort_by(|a, b| b.opened_at.cmp(&a.opened_at));
        Ok(positions)
    }

    async fn update_position_in_redis(&self, position: &TradePosition) -> Result<()> {
        let mut conn =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await?;
        //read all, replace matching id, rewrite
        let raw: Vec<String> = conn.lrange(REDIS_TRADE_POSITIONS, 0, 199).await?;
        let mut entries: Vec<String> = Vec::with_capacity(raw.len());
        for entry in &raw {
            if let Ok(pos) = serde_json::from_str::<TradePosition>(entry) {
                if pos.id == position.id {
                    entries.push(serde_json::to_string(position)?);
                    continue;
                }
            }
            entries.push(entry.clone());
        }
        conn.del::<_, ()>(REDIS_TRADE_POSITIONS).await?;
        for e in entries.iter().rev() {
            conn.lpush::<_, _, ()>(REDIS_TRADE_POSITIONS, e).await?;
        }
        Ok(())
    }

    //── position-based loop ──────────────────────────────────────

    pub async fn start_position_loop(
        self: &Arc<Self>,
        wallet_label: String,
        chain: String,
    ) -> Result<()> {
        //use a synthetic uuid derived from wallet+chain for dedup
        let loop_key = make_loop_key(&wallet_label, &chain);

        let mut loops = self.active_loops.lock().await;
        if loops.contains_key(&loop_key) {
            anyhow::bail!("trade-up position loop already running for {wallet_label} on {chain}");
        }

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let orch = Arc::clone(self);
        let wallet = wallet_label.clone();
        let ch = chain.clone();

        let handle = tokio::spawn(async move {
            run_position_loop(orch, wallet, ch, cancel_rx).await;
        });

        loops.insert(loop_key, (cancel_tx, handle));
        tracing::info!("trade-up position loop started for {wallet_label} on {chain}");
        Ok(())
    }

    pub async fn stop_all_loops(&self) -> Result<()> {
        let mut loops = self.active_loops.lock().await;
        for (id, (cancel_tx, _handle)) in loops.drain() {
            let _ = cancel_tx.send(true);
            tracing::info!("trade-up loop stop sent for {id}");
        }
        Ok(())
    }

    pub async fn get_loop_phase(&self) -> Option<LoopPhase> {
        let conn = redis::Client::get_multiplexed_async_connection(self.redis.as_ref())
            .await
            .ok();
        if let Some(mut conn) = conn {
            let raw: Option<String> = conn
                .get::<_, Option<String>>("vespra:trade_up:loop_phase")
                .await
                .ok()
                .flatten();
            raw.and_then(|s| serde_json::from_str(&format!("\"{}\"", s)).ok())
        } else {
            None
        }
    }

    async fn set_loop_phase(&self, phase: &LoopPhase) {
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let _: Result<(), _> = conn
                .set::<_, _, ()>("vespra:trade_up:loop_phase", phase.to_string())
                .await;
        }
    }
}

//─── background loop task ────────────────────────────────────────

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

    //persist initial state
    orch.persist_loop_state(wallet_id, 0, capital, "started", true)
        .await;

    loop {
        //check kill switch
        if orch.is_killed() {
            tracing::warn!("kill switch active — halting loop for wallet {wallet_id}");
            break;
        }

        //check cancellation
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

        //persist state after each cycle
        let status_str = match result.status {
            CycleStatus::Executed => "executed",
            CycleStatus::Hold => "hold",
            CycleStatus::Exit => "exit",
            CycleStatus::Error => "error",
        };
        orch.persist_loop_state(wallet_id, cycle, capital, status_str, true)
            .await;

        //persist cycle to history list (every cycle, not just executed swaps)
        let _ = orch.persist_cycle_to_history(wallet_id, &result).await;

        //stop-loss: check drawdown from peak
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

        //exit status ends the loop
        if result.status == CycleStatus::Exit {
            break;
        }

        //wait for next cycle or cancellation
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

    //persist final state
    orch.persist_loop_state(wallet_id, cycle, capital, "stopped", false)
        .await;

    //clean up from active loops
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

//─── position-based state machine loop ──────────────────────────

async fn run_position_loop(
    orch: Arc<TradeUpOrchestrator>,
    wallet_label: String,
    chain: String,
    mut cancel_rx: watch::Receiver<bool>,
) {
    let loop_key = make_loop_key(&wallet_label, &chain);

    tracing::info!(
        wallet = %wallet_label,
        chain = %chain,
        "position loop starting — state machine: SCOUTING → RISK_CHECK → ENTERING → MONITORING → EXITING → COMPOUNDING"
    );

    loop {
        if orch.is_killed() || *cancel_rx.borrow() {
            tracing::info!("position loop cancelled for {wallet_label}");
            break;
        }

        //── scouting ─────────────────────────────────────────
        orch.set_loop_phase(&LoopPhase::Scouting).await;
        tracing::info!("[{wallet_label}] SCOUTING on {chain}");

        let chains = vec![chain.clone()];
        let pools = match orch.pool_fetcher.fetch(&chains).await {
            Ok(p) if !p.is_empty() => p,
            _ => {
                tracing::warn!("[{wallet_label}] no pools — waiting 5min");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
        };

        let scout_ctx = ScoutContext {
            wallet_id: loop_key,
            mode: "momentum".to_string(),
            pools: pools.clone(),
            chains: chains.clone(),
        };
        let best = match orch.scout.analyze(&scout_ctx).await {
            Ok(ScoutDecision::Opportunities(opps)) => {
                match opps
                    .into_iter()
                    .max_by(|a, b| {
                        a.momentum_score
                            .partial_cmp(&b.momentum_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                    Some(o) if o.momentum_score >= 0.6 => o,
                    _ => {
                        tracing::info!("[{wallet_label}] no high-conviction opportunity — retry in 5min");
                        wait_or_cancel(&mut cancel_rx, 300).await;
                        continue;
                    }
                }
            }
            _ => {
                tracing::warn!("[{wallet_label}] scout error/no opportunities — retry in 5min");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
        };

        tracing::info!(
            "[{wallet_label}] scout picked: {} {} momentum={:.2}",
            best.protocol,
            best.pool,
            best.momentum_score
        );

        //── risk_check ───────────────────────────────────────
        if orch.is_killed() || *cancel_rx.borrow() { break; }
        orch.set_loop_phase(&LoopPhase::RiskCheck).await;
        tracing::info!("[{wallet_label}] RISK_CHECK for {}", best.protocol);

        let protocol_data = orch
            .protocol_fetcher
            .fetch_protocol(&best.protocol)
            .await
            .unwrap_or_default();

        let risk_ctx = RiskContext {
            chain: best.chain.clone(),
            opportunity: best.clone(),
            protocol_data,
        };
        let risk_decision = match orch.risk.assess(&risk_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[{wallet_label}] risk error: {e} — retry in 5min");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
        };

        if risk_decision.is_blocked() {
            tracing::info!("[{wallet_label}] risk blocked — waiting 5min then re-scouting");
            wait_or_cancel(&mut cancel_rx, 300).await;
            continue;
        }
        tracing::info!("[{wallet_label}] risk passed: {:?}", risk_decision);

        //── entering ─────────────────────────────────────────
        if orch.is_killed() || *cancel_rx.borrow() { break; }
        orch.set_loop_phase(&LoopPhase::Entering).await;
        tracing::info!("[{wallet_label}] ENTERING position");

        //check wallet balance
        let wallets = orch
            .wallet_fetcher
            .fetch_wallets(&chain)
            .await
            .unwrap_or_default();
        let wallet_state = wallets
            .iter()
            .find(|w| w.address.contains(&wallet_label) || w.chain == chain);
        let wallet_balance = wallet_state.map(|w| w.balance_eth).unwrap_or(0.0);
        let gas_reserve = orch.config.trade_up_gas_reserve_eth;

        if wallet_balance <= gas_reserve {
            tracing::warn!(
                "[{wallet_label}] insufficient balance {wallet_balance} ETH <= gas reserve {gas_reserve} ETH — aborting"
            );
            break;
        }

        let max_eth = orch.config.trade_up_max_eth;
        let position_eth = f64::min(wallet_balance - gas_reserve, max_eth);

        //get quote for entry
        let chain_id = orch
            .chain_registry
            .chain_id(&chain.to_lowercase())
            .unwrap_or(8453);
        let amount_wei = format!("{:.0}", position_eth * 1e18);
        let quote = orch
            .quote_fetcher
            .fetch_quote("WETH", &best.pool, &amount_wei, chain_id)
            .await
            .unwrap_or_default();

        //execute entry swap via trader + executor
        let trader_ctx = TraderContext {
            chain: best.chain.clone(),
            opportunity: best.clone(),
            quote: quote.clone(),
            capital_eth: position_eth,
            risk_score: risk_decision.score().clone(),
            min_gain_pct: orch.config.trade_up_min_gain_pct,
            max_eth,
        };
        let trader_decision = match orch.trader.evaluate(&trader_ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::error!("[{wallet_label}] trader error: {e} — retry in 5min");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
        };

        let (token_out, amount_in_wei_str) = match &trader_decision {
            TraderDecision::Swap { token_out, amount_in_wei, .. } => {
                (token_out.clone(), amount_in_wei.clone())
            }
            TraderDecision::Hold { reason } => {
                tracing::info!("[{wallet_label}] trader says hold: {reason} — retry in 5min");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
            TraderDecision::Exit { reason } => {
                tracing::info!("[{wallet_label}] trader says exit: {reason} — retry in 5min");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
        };

        //execute entry
        let wallet_uuid = wallet_state
            .map(|w| w.wallet_id)
            .unwrap_or_else(Uuid::new_v4);
        let exec_result = match orch
            .executor
            .execute(wallet_uuid, "WETH", &token_out, &amount_in_wei_str, &chain)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[{wallet_label}] executor error: {e}");
                wait_or_cancel(&mut cancel_rx, 300).await;
                continue;
            }
        };

        if exec_result.status != ExecutorStatus::Success {
            tracing::error!(
                "[{wallet_label}] entry tx failed: {:?}",
                exec_result.error
            );
            wait_or_cancel(&mut cancel_rx, 300).await;
            continue;
        }

        //create position
        let token_amount = quote.amount_out_wei.parse::<f64>().unwrap_or(0.0);
        let position_id = Uuid::new_v4().to_string();
        let mut position = TradePosition {
            id: position_id.clone(),
            wallet: wallet_label.clone(),
            chain: chain.clone(),
            token_address: best.pool.clone(),
            token_symbol: best.protocol.clone(),
            entry_price_usd: best.price_usd,
            entry_eth: position_eth,
            token_amount,
            opened_at: chrono::Utc::now().timestamp(),
            status: PositionStatus::Open,
            exit_price_usd: None,
            exit_eth: None,
            gas_cost_eth: None,
            net_gain_eth: None,
            exit_reason: None,
            closed_at: None,
        };

        let _ = orch.save_position(&position).await;
        let _ = orch.set_active_position(&position_id).await;
        tracing::info!(
            "[{wallet_label}] position opened: {} {:.4} ETH → {} at {:.4} USD",
            position_id,
            position_eth,
            best.protocol,
            best.price_usd
        );

        //── monitoring (poll every 5 min) ────────────────────
        orch.set_loop_phase(&LoopPhase::Monitoring).await;
        #[allow(unused_assignments)]
        let mut exit_reason: Option<String> = None;
        let target_gain_pct = orch.config.trade_up_target_gain_pct;
        let stop_loss_pct = orch.config.trade_up_stop_loss_pct;

        loop {
            if orch.is_killed() || *cancel_rx.borrow() {
                exit_reason = Some("cancelled".into());
                break;
            }

            //wait 5min between checks
            if wait_or_cancel(&mut cancel_rx, 300).await {
                exit_reason = Some("cancelled".into());
                break;
            }

            tracing::info!("[{wallet_label}] MONITORING position {position_id}");

            //fetch current price
            let price_data = orch
                .price_oracle
                .fetch(&position.token_address, &chain)
                .await
                .unwrap_or_default();
            let current_price = price_data.price_usd;

            if current_price <= 0.0 {
                tracing::warn!("[{wallet_label}] price unavailable — skipping check");
                continue;
            }

            let gain_pct = ((current_price - position.entry_price_usd)
                / position.entry_price_usd)
                * 100.0;
            tracing::info!(
                "[{wallet_label}] price={:.4} entry={:.4} gain={:.2}%",
                current_price,
                position.entry_price_usd,
                gain_pct
            );

            //check gain/loss thresholds
            if gain_pct >= target_gain_pct {
                exit_reason = Some("target_gain".into());
                break;
            }
            if gain_pct <= -stop_loss_pct {
                exit_reason = Some("stop_loss".into());
                break;
            }

            //sentinel assessment
            if let Ok(assessment) = orch
                .sentinel
                .monitor_position(&position, current_price)
                .await
            {
                if assessment.is_exit() {
                    tracing::info!(
                        "[{wallet_label}] sentinel says {}: {}",
                        assessment.action,
                        assessment.reasoning
                    );
                    exit_reason = Some(assessment.action.clone());
                    break;
                }
            }

            //check for better opportunity
            let scout_check = ScoutContext {
                wallet_id: loop_key,
                mode: "momentum".to_string(),
                pools: pools.clone(),
                chains: chains.clone(),
            };
            if let Ok(ScoutDecision::Opportunities(opps)) =
                orch.scout.analyze(&scout_check).await
            {
                if let Some(alt) = opps
                    .iter()
                    .filter(|o| o.pool != position.token_address)
                    .max_by(|a, b| {
                        a.momentum_score
                            .partial_cmp(&b.momentum_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                {
                    if alt.momentum_score > best.momentum_score + 0.1 {
                        tracing::info!(
                            "[{wallet_label}] better opportunity: {} score={:.2} vs {:.2}",
                            alt.protocol,
                            alt.momentum_score,
                            best.momentum_score
                        );
                        exit_reason = Some("better_opportunity".into());
                        break;
                    }
                }
            }
        }

        //── exiting ──────────────────────────────────────────
        if orch.is_killed() && exit_reason.is_none() {
            break;
        }
        orch.set_loop_phase(&LoopPhase::Exiting).await;
        let reason = exit_reason.unwrap_or_else(|| "unknown".into());
        tracing::info!("[{wallet_label}] EXITING position — reason: {reason}");

        position.status = PositionStatus::Exiting;
        let _ = orch.update_position_in_redis(&position).await;

        //get exit quote token→eth
        let exit_amount_wei = format!("{:.0}", position.token_amount);
        let exit_quote = orch
            .quote_fetcher
            .fetch_quote(&position.token_address, "WETH", &exit_amount_wei, chain_id)
            .await
            .unwrap_or_default();

        //execute exit swap
        let exit_result = orch
            .executor
            .execute(
                wallet_uuid,
                &position.token_address,
                "WETH",
                &exit_amount_wei,
                &chain,
            )
            .await;

        let exit_price = orch
            .price_oracle
            .fetch(&position.token_address, &chain)
            .await
            .map(|p| p.price_usd)
            .unwrap_or(0.0);

        let exit_eth = exit_quote.amount_out_wei.parse::<f64>().unwrap_or(0.0) / 1e18;
        let gas_cost = 0.002; // Estimate; actual comes from tx receipt
        let net_gain = exit_eth - position.entry_eth - gas_cost;

        position.status = match &exit_result {
            Ok(r) if r.status == ExecutorStatus::Success => PositionStatus::Closed,
            _ => PositionStatus::Failed,
        };
        position.exit_price_usd = Some(exit_price);
        position.exit_eth = Some(exit_eth);
        position.gas_cost_eth = Some(gas_cost);
        position.net_gain_eth = Some(net_gain);
        position.exit_reason = Some(reason.clone());
        position.closed_at = Some(chrono::Utc::now().timestamp());

        let _ = orch.update_position_in_redis(&position).await;
        let _ = orch.clear_active_position().await;

        tracing::info!(
            "[{wallet_label}] position closed: entry={:.4} exit={:.4} net_gain={:.4} ETH reason={reason}",
            position.entry_eth,
            exit_eth,
            net_gain
        );

        //── compounding ──────────────────────────────────────
        orch.set_loop_phase(&LoopPhase::Compounding).await;
        tracing::info!(
            "[{wallet_label}] COMPOUNDING — P&L: {:.4} ETH ({:.2}%) — looping back in 30s",
            net_gain,
            if position.entry_eth > 0.0 {
                (net_gain / position.entry_eth) * 100.0
            } else {
                0.0
            }
        );

        if wait_or_cancel(&mut cancel_rx, 30).await {
            break;
        }
    }

    //final cleanup
    orch.set_loop_phase(&LoopPhase::Idle).await;
    let mut loops = orch.active_loops.lock().await;
    loops.remove(&loop_key);
    tracing::info!("position loop ended for {wallet_label} on {chain}");
}

///wait for `secs` seconds or until cancel signal. returns true if cancelled.
async fn wait_or_cancel(cancel_rx: &mut watch::Receiver<bool>, secs: u64) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => false,
        _ = cancel_rx.changed() => *cancel_rx.borrow(),
    }
}
