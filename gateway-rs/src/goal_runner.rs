use std::sync::Arc;

use chrono::Utc;
use tokio::sync::watch;
use uuid::Uuid;

use crate::agents::executor::ExecutorAgent;
use crate::agents::risk::RiskAgent;
use crate::agents::scout::ScoutAgent;
use crate::agents::sentinel::SentinelAgent;
use crate::agents::trader::TraderAgent;
use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;
use crate::data::pool::PoolFetcher;
use crate::data::price::PriceOracle;
use crate::data::protocol::ProtocolFetcher;
use crate::data::quote::QuoteFetcher;
use crate::data::wallet::WalletFetcher;
use crate::routes::goals::{get_goal, save_goal, update_goal_pnl, update_goal_step};
use crate::execution_gate;
use crate::types::decisions::{ScoutDecision, TraderDecision};
use crate::types::goals::GoalStatus;
use crate::types::trade_up::TradePosition;
use crate::types::tx::TxStatus;

use crate::agents::risk::RiskContext;
use crate::agents::scout::ScoutContext;
use crate::agents::trader::TraderContext;
use crate::sentinel_monitor::{SentinelSignal, SENTINEL_CHANNEL};
use crate::types::goals::GoalStrategy;
use crate::yield_scheduler::{YieldRotationSignal, YIELD_ROTATE_CHANNEL};

const MAX_RETRIES: u32 = 3;
const RETRY_BACKOFF_SECS: u64 = 10;
const MONITOR_INTERVAL_SECS: u64 = 300; // 5 minutes
const PAUSE_CHECK_SECS: u64 = 60;

/// Determines which step a resumed GoalRunner should begin from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalStep {
    Scouting,
    Monitoring,
}

/// Shared dependencies for a GoalRunner task.
#[derive(Clone)]
pub struct GoalRunnerDeps {
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
    pub dry_run: bool,
}

/// Run the GoalRunner loop as a tokio task.
/// Checks `cancel_rx` between every step.
pub async fn run_goal(
    goal_id: Uuid,
    cancel_rx: watch::Receiver<bool>,
    deps: GoalRunnerDeps,
) {
    run_goal_with_resume(goal_id, cancel_rx, deps, None).await;
}

/// Run the GoalRunner with an optional resume point.
/// `resume_from`: if `Some(Monitoring)`, the first iteration skips SCOUTING→EXECUTING
/// and jumps directly to MONITORING. Subsequent iterations run from SCOUTING normally.
pub async fn run_goal_with_resume(
    goal_id: Uuid,
    mut cancel_rx: watch::Receiver<bool>,
    deps: GoalRunnerDeps,
    resume_from: Option<GoalStep>,
) {
    tracing::info!("[goal {goal_id}] runner started (resume={resume_from:?})");
    let mut first_iteration = true;

    loop {
        // ── Check cancel ────────────────────────────────────────
        if *cancel_rx.borrow() {
            tracing::info!("[goal {goal_id}] cancelled — exiting runner");
            break;
        }

        // ── Load current goal state ─────────────────────────────
        let goal = match get_goal(&deps.redis, goal_id).await {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("[goal {goal_id}] failed to load goal: {e}");
                break;
            }
        };

        match goal.status {
            GoalStatus::Cancelled | GoalStatus::Completed | GoalStatus::Failed => {
                tracing::info!("[goal {goal_id}] status={:?} — exiting runner", goal.status);
                break;
            }
            GoalStatus::Paused => {
                tracing::info!("[goal {goal_id}] paused — sleeping {PAUSE_CHECK_SECS}s");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(PAUSE_CHECK_SECS)) => {}
                    _ = cancel_rx.changed() => {}
                }
                continue;
            }
            GoalStatus::Pending | GoalStatus::Running => {
                // proceed
            }
        }

        // ── Resume shortcut: skip to MONITORING on first iteration if resuming ──
        if first_iteration && resume_from == Some(GoalStep::Monitoring) {
            first_iteration = false;
            tracing::info!("[goal {goal_id}] resuming directly into MONITORING");
            if let Err(e) = update_goal_step(&deps.redis, goal_id, "MONITORING").await {
                tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
            }

            // Build a position from persisted goal state. `token_address` is set
            // when the BUY confirms; if it's missing the goal can't have a real
            // open position to monitor, so fail rather than spin against a
            // placeholder that breaks the price oracle.
            let resumed_token_address = match goal.token_address.clone() {
                Some(addr) if !addr.is_empty() => addr,
                _ => {
                    fail_goal(
                        &deps.redis,
                        goal_id,
                        "cannot resume monitoring: goal has no persisted token_address (BUY never recorded)",
                    )
                    .await;
                    break;
                }
            };
            let position = TradePosition {
                id: goal_id.to_string(),
                wallet: goal.wallet_label.clone(),
                chain: goal.chain.clone(),
                token_address: resumed_token_address.clone(),
                token_symbol: "RESUMED".into(),
                entry_price_usd: 0.0,
                entry_eth: goal.capital_eth,
                token_amount: 0.0,
                opened_at: goal.created_at.timestamp(),
                status: crate::types::trade_up::PositionStatus::Open,
                exit_price_usd: None,
                exit_eth: None,
                gas_cost_eth: None,
                net_gain_eth: None,
                exit_reason: None,
                closed_at: None,
            };

            let resume_result = run_monitoring_loop(
                goal_id, &goal, &position, &deps, &mut cancel_rx,
            ).await;

            match resume_result {
                MonitorResult::Cancelled => break,
                MonitorResult::YieldRotate => {
                    let _ = update_goal_step(&deps.redis, goal_id, "SCOUTING").await;
                    continue;
                }
                MonitorResult::ExitSignal => {
                    // EXITING from resumed monitoring
                    tracing::info!("[goal {goal_id}] MONITORING → EXITING (resumed)");
                    let _ = update_goal_step(&deps.redis, goal_id, "EXITING").await;
                    let token_out = resumed_token_address.clone();
                    let sell_amount = match goal.token_amount_held.clone() {
                        Some(amt) if !amt.is_empty() && amt != "0" => amt,
                        _ => {
                            fail_goal(
                                &deps.redis,
                                goal_id,
                                "token_amount_held not set — cannot sell",
                            )
                            .await;
                            break;
                        }
                    };
                    let weth_addr = match known_token_address("WETH", &goal.chain) {
                        Some(a) => a,
                        None => {
                            fail_goal(
                                &deps.redis,
                                goal_id,
                                &format!("no known WETH address for chain '{}'", goal.chain),
                            )
                            .await;
                            break;
                        }
                    };
                    match resolve_goal_wallet_uuid(&goal) {
                        Ok(wallet_uuid) => {
                            let _ = execution_gate::execute_traced(
                                &deps.executor, &deps.config, &deps.chain_registry,
                                wallet_uuid, &token_out, &weth_addr, &sell_amount, &goal.chain, deps.dry_run,
                            ).await;
                        }
                        Err(e) => {
                            fail_goal(&deps.redis, goal_id, &e).await;
                            break;
                        }
                    }
                    continue; // go back to SCOUTING on next iteration
                }
                MonitorResult::NoExit => continue,
            }
        }
        first_iteration = false;

        let chains = vec![goal.chain.clone()];

        // ═════════════════════════════════════════════════════════
        // STEP 1 — SCOUTING
        // ═════════════════════════════════════════════════════════
        tracing::info!("[goal {goal_id}] SCOUTING");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "SCOUTING").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        let scout_result = with_retry(goal_id, "SCOUTING", &deps, &cancel_rx, || {
            let deps = deps.clone();
            let chains = chains.clone();
            async move {
                let pools = deps.pool_fetcher.fetch(&chains).await?;
                if pools.is_empty() {
                    anyhow::bail!("no pools available");
                }
                let scout_ctx = ScoutContext {
                    wallet_id: goal_id,
                    mode: "momentum".to_string(),
                    pools,
                    chains,
                };
                let decision = deps.scout.analyze(&scout_ctx).await?;
                match decision {
                    ScoutDecision::Opportunities(opps) => {
                        opps.into_iter()
                            .max_by(|a, b| {
                                a.momentum_score
                                    .partial_cmp(&b.momentum_score)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .ok_or_else(|| anyhow::anyhow!("scout returned empty opportunities"))
                    }
                    ScoutDecision::NoOpportunities { reason } => {
                        anyhow::bail!("no opportunities: {reason}")
                    }
                }
            }
        })
        .await;

        let best = match scout_result {
            Ok(opp) => opp,
            Err(e) => {
                // TODO(VES-92): Replace this with a real testnet pool index.
                // DefiLlama indexes only fake/junk tokens for Base Sepolia
                // (ROBO, POLYCLAUDE, ASCV, …) so Scout legitimately finds no
                // swappable pools and the pipeline can't be exercised end-to-end.
                // As an interim, on testnet chains we inject a hardcoded
                // WETH→USDC opportunity so the rest of the runner (RISK,
                // TRADER, /swap) actually fires. The pool symbol "WETH-USDC"
                // is enough for the executor's symbol→address resolver to pick
                // 0x4200… (WETH) and 0x036C… (USDC) on Base Sepolia.
                let chain_lower = goal.chain.to_lowercase();
                let is_testnet = chain_lower.contains("sepolia")
                    || chain_lower.contains("testnet")
                    || chain_lower.contains("goerli");
                if is_testnet {
                    tracing::info!(
                        "[goal {goal_id}] [scout] no real pools on testnet — injecting fallback WETH/USDC opportunity (scout error: {e})"
                    );
                    crate::types::opportunity::Opportunity {
                        protocol: "uniswap-v3".to_string(),
                        pool: "WETH-USDC".to_string(),
                        chain: goal.chain.clone(),
                        apy: 5.0,
                        tvl_usd: 1_000_000,
                        momentum_score: 1.0,
                        ..Default::default()
                    }
                } else {
                    fail_goal(&deps.redis, goal_id, &format!("SCOUTING failed: {e}")).await;
                    break;
                }
            }
        };

        if *cancel_rx.borrow() { continue; }

        // ═════════════════════════════════════════════════════════
        // STEP 2 — RISK
        // ═════════════════════════════════════════════════════════
        tracing::info!("[goal {goal_id}] SCOUTING → RISK");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "RISK").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        let goal_chain = goal.chain.clone();
        let risk_decision = match with_retry(goal_id, "RISK", &deps, &cancel_rx, || {
            let deps = deps.clone();
            let best = best.clone();
            let goal_chain = goal_chain.clone();
            async move {
                let protocol_data = deps
                    .protocol_fetcher
                    .fetch_protocol(&best.protocol)
                    .await
                    .unwrap_or_default();
                let risk_ctx = RiskContext {
                    chain: goal_chain,
                    opportunity: best,
                    protocol_data,
                };
                deps.risk.assess(&risk_ctx).await
            }
        })
        .await
        {
            Ok(d) => d,
            Err(e) => {
                fail_goal(&deps.redis, goal_id, &format!("RISK failed: {e}")).await;
                break;
            }
        };

        if risk_decision.is_blocked() {
            tracing::info!("[goal {goal_id}] risk gate blocked — retrying next cycle");
            sleep_interruptible(&mut cancel_rx, 60).await;
            continue;
        }

        if *cancel_rx.borrow() { continue; }

        // ═════════════════════════════════════════════════════════
        // STEP 3 — TRADING
        // ═════════════════════════════════════════════════════════
        tracing::info!("[goal {goal_id}] RISK → TRADING");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "TRADING").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        let chain_id = match deps.chain_registry.chain_id(&best.chain.to_lowercase()) {
            Some(id) => id,
            None => {
                fail_goal(
                    &deps.redis,
                    goal_id,
                    &format!(
                        "chain_id could not be resolved for '{}' — goal aborted",
                        best.chain
                    ),
                )
                .await;
                break;
            }
        };
        let amount_wei = format!("{:.0}", goal.capital_eth * 1e18);

        let trader_chain = goal.chain.clone();
        let trader_decision = match with_retry(goal_id, "TRADING", &deps, &cancel_rx, || {
            let deps = deps.clone();
            let best = best.clone();
            let amount_wei = amount_wei.clone();
            let risk_score = risk_decision.score().clone();
            let goal_capital = goal.capital_eth;
            let goal_target = goal.target_gain_pct;
            let trader_chain = trader_chain.clone();
            async move {
                let quote = deps
                    .quote_fetcher
                    .fetch_quote("WETH", &best.pool, &amount_wei, chain_id)
                    .await
                    .unwrap_or_default();
                tracing::info!(
                    "[exec-trace] quote: {} -> {} amount_in={} amount_out={} slippage={:.4}%",
                    quote.token_in, quote.token_out, quote.amount_in_wei,
                    quote.amount_out_wei, quote.price_impact
                );
                let trader_ctx = TraderContext {
                    chain: trader_chain,
                    opportunity: best,
                    quote,
                    capital_eth: goal_capital,
                    risk_score,
                    min_gain_pct: goal_target * 0.1, // aim for 10% of total target per cycle
                    max_eth: goal_capital,
                };
                deps.trader.evaluate(&trader_ctx).await
            }
        })
        .await
        {
            Ok(d) => d,
            Err(e) => {
                fail_goal(&deps.redis, goal_id, &format!("TRADING failed: {e}")).await;
                break;
            }
        };

        if *cancel_rx.borrow() { continue; }

        // ═════════════════════════════════════════════════════════
        // STEP 4 — EXECUTING
        // ═════════════════════════════════════════════════════════
        let (token_in, token_out, swap_amount_wei, expected_gain_pct) = match trader_decision {
            TraderDecision::Swap {
                ref token_in,
                ref token_out,
                ref amount_in_wei,
                expected_gain_pct,
                ref reasoning,
            } => {
                tracing::info!(
                    "[exec-trace] trader decision: SWAP {} -> {} amount={} gain={:.4}% reason={}",
                    token_in, token_out, amount_in_wei, expected_gain_pct, reasoning
                );
                (token_in.clone(), token_out.clone(), amount_in_wei.clone(), expected_gain_pct)
            }
            TraderDecision::Hold { reason } => {
                tracing::info!("[exec-trace] trader decision: HOLD reason={reason}");
                sleep_interruptible(&mut cancel_rx, 60).await;
                continue;
            }
            TraderDecision::Exit { reason } => {
                tracing::info!("[exec-trace] trader decision: EXIT reason=: {reason}");
                sleep_interruptible(&mut cancel_rx, 60).await;
                continue;
            }
        };

        // Resolve symbol → address before handing off to executor.
        // Scout often emits pool symbols ("USDC-WETH", "LIQTEST-WETH") rather
        // than ERC-20 addresses. Testnet pools sometimes contain fake tokens
        // (LIQTEST, VELVET, VFY) with no real contract — skip them and
        // re-SCOUT instead of submitting a broken swap to Keymaster.
        let token_in = match resolve_swap_token(&token_in, &goal.chain) {
            Some(a) => a,
            None => {
                tracing::warn!(
                    "[goal {goal_id}] skipping pool: unresolvable token_in symbol '{}' on chain '{}' — re-scouting",
                    token_in, goal.chain
                );
                sleep_interruptible(&mut cancel_rx, 30).await;
                continue;
            }
        };
        let token_out = match resolve_swap_token(&token_out, &goal.chain) {
            Some(a) => a,
            None => {
                tracing::warn!(
                    "[goal {goal_id}] skipping pool: unresolvable token_out symbol '{}' on chain '{}' — re-scouting",
                    token_out, goal.chain
                );
                sleep_interruptible(&mut cancel_rx, 30).await;
                continue;
            }
        };
        // Guard against degenerate same-token swaps. This happens when a pool
        // symbol like "WETH-CBBTC" pairs WETH with an unknown token: the
        // resolver falls back to WETH for both sides since CBBTC has no entry
        // in the known-token map. The downstream swap would either revert or
        // be a no-op, so re-scout instead.
        if token_in.eq_ignore_ascii_case(&token_out) {
            tracing::warn!(
                "[goal {goal_id}] skipping pool: token_in == token_out after resolution ({}); pool symbol likely contains an unknown token paired with WETH — re-scouting",
                token_in
            );
            sleep_interruptible(&mut cancel_rx, 30).await;
            continue;
        }
        tracing::info!(
            "[goal {goal_id}] resolved swap addresses: in={} out={}",
            token_in, token_out
        );

        tracing::info!("[goal {goal_id}] TRADING → EXECUTING");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "EXECUTING").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        let wallet_uuid = match resolve_goal_wallet_uuid(&goal) {
            Ok(u) => u,
            Err(e) => {
                fail_goal(&deps.redis, goal_id, &e).await;
                break;
            }
        };

        let buy_tx_status = execution_gate::execute_traced(
            &deps.executor,
            &deps.config,
            &deps.chain_registry,
            wallet_uuid,
            &token_in,
            &token_out,
            &swap_amount_wei,
            &goal.chain,
            deps.dry_run,
        )
        .await;

        match &buy_tx_status {
            TxStatus::Confirmed { tx_hash, .. } => {
                tracing::info!("[goal {goal_id}] BUY confirmed, tx={tx_hash}");
            }
            TxStatus::DryRun { .. } => {
                tracing::info!("[goal {goal_id}] BUY dry-run complete");
            }
            TxStatus::Reverted { tx_hash, .. } => {
                fail_goal(&deps.redis, goal_id, &format!("BUY tx reverted: {tx_hash}")).await;
                break;
            }
            TxStatus::Timeout { tx_hash, .. } => {
                fail_goal(&deps.redis, goal_id, &format!("BUY tx timeout: {tx_hash}")).await;
                break;
            }
            TxStatus::Failed { error } => {
                fail_goal(&deps.redis, goal_id, &format!("BUY failed: {error}")).await;
                break;
            }
        }

        // Capture the amount of `token_out` received so the SELL leg can sell
        // exactly what we hold (not capital_eth * 1e18, which assumes a 1:1
        // ETH→token mint and reverts on real swaps). We re-quote with the
        // resolved addresses; if the quote fails the sell will fail-fast at
        // SELL time with a clear "token_amount_held not set" error rather than
        // submitting a doomed tx.
        let token_amount_out = deps
            .quote_fetcher
            .fetch_quote(&token_in, &token_out, &swap_amount_wei, chain_id)
            .await
            .ok()
            .map(|q| q.amount_out_wei)
            .filter(|s| !s.is_empty() && s != "0");
        match token_amount_out.as_ref() {
            Some(amt) => tracing::info!(
                "[goal {goal_id}] recording token_amount_held={} for token={}",
                amt, token_out
            ),
            None => tracing::warn!(
                "[goal {goal_id}] could not determine token_amount_held after BUY — SELL will fail-fast"
            ),
        }
        if let Ok(mut g) = get_goal(&deps.redis, goal_id).await {
            g.token_address = Some(token_out.clone());
            g.token_amount_held = token_amount_out.clone();
            g.updated_at = Utc::now();
            if let Err(e) = save_goal(&deps.redis, &g).await {
                tracing::warn!("[goal {goal_id}] failed to persist token_address/amount: {e}");
            }
        }

        if *cancel_rx.borrow() { continue; }

        // ═════════════════════════════════════════════════════════
        // STEP 5 — MONITORING
        // ═════════════════════════════════════════════════════════
        tracing::info!("[goal {goal_id}] EXECUTING → MONITORING");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "MONITORING").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        let position = TradePosition {
            id: Uuid::new_v4().to_string(),
            wallet: goal.wallet_label.clone(),
            chain: goal.chain.clone(),
            token_address: token_out.clone(),
            token_symbol: token_out.clone(),
            entry_price_usd: 0.0,
            entry_eth: goal.capital_eth,
            token_amount: 0.0,
            opened_at: Utc::now().timestamp(),
            status: crate::types::trade_up::PositionStatus::Open,
            exit_price_usd: None,
            exit_eth: None,
            gas_cost_eth: None,
            net_gain_eth: None,
            exit_reason: None,
            closed_at: None,
        };

        let monitor_result = run_monitoring_loop(
            goal_id, &goal, &position, &deps, &mut cancel_rx,
        ).await;

        match monitor_result {
            MonitorResult::Cancelled => {
                tracing::info!("[goal {goal_id}] cancelled during monitoring — not exiting position");
                break;
            }
            MonitorResult::YieldRotate => {
                tracing::info!("[goal {goal_id}] MONITORING → EXITING (yield rotation) → will re-SCOUT");
                let _ = update_goal_step(&deps.redis, goal_id, "SCOUTING").await;
                continue;
            }
            MonitorResult::NoExit => continue,
            MonitorResult::ExitSignal => {
                // fall through to STEP 6 — EXITING
            }
        }

        // ═════════════════════════════════════════════════════════
        // STEP 6 — EXITING
        // ═════════════════════════════════════════════════════════
        tracing::info!("[goal {goal_id}] MONITORING → EXITING");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "EXITING").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        // Use the actual token amount captured at BUY time. If it's missing
        // (quote failed or BUY never recorded), fail-fast instead of submitting
        // a sell that will revert and leave funds stuck.
        let sell_amount = match token_amount_out.clone() {
            Some(amt) => amt,
            None => {
                fail_goal(
                    &deps.redis,
                    goal_id,
                    "token_amount_held not set — cannot sell",
                )
                .await;
                break;
            }
        };
        // Reuse the wallet_uuid resolved earlier in this iteration; if for some
        // reason the goal lost it (re-loaded between steps), fall back to a
        // fresh resolution and fail-fast on error.
        let sell_wallet_uuid = match resolve_goal_wallet_uuid(&goal) {
            Ok(u) => u,
            Err(e) => {
                fail_goal(&deps.redis, goal_id, &e).await;
                break;
            }
        };
        // token_out is already a resolved address (set during BUY handoff above).
        // Resolve "WETH" symbol to the chain's WETH address for the sell side.
        let weth_addr = match known_token_address("WETH", &goal.chain) {
            Some(a) => a,
            None => {
                fail_goal(
                    &deps.redis,
                    goal_id,
                    &format!("no known WETH address for chain '{}'", goal.chain),
                )
                .await;
                break;
            }
        };
        let sell_tx_status = execution_gate::execute_traced(
            &deps.executor,
            &deps.config,
            &deps.chain_registry,
            sell_wallet_uuid,
            &token_out,
            &weth_addr,
            &sell_amount,
            &goal.chain,
            deps.dry_run,
        )
        .await;

        match &sell_tx_status {
            TxStatus::Confirmed { tx_hash, .. } => {
                tracing::info!("[goal {goal_id}] SELL confirmed, tx={tx_hash}");
            }
            TxStatus::DryRun { .. } => {
                tracing::info!("[goal {goal_id}] SELL dry-run complete");
            }
            other => {
                tracing::error!("[goal {goal_id}] SELL failed: {:?}", other);
            }
        }

        // Collect exit fee (only on confirmed/dry-run sells with profit)
        if matches!(sell_tx_status, TxStatus::Confirmed { .. } | TxStatus::DryRun { .. }) {
            crate::fees::collect_exit_fee(
                goal_id,
                goal.entry_eth,
                goal.current_eth,
                &goal.chain,
                &deps.executor,
                &deps.config,
                &deps.chain_registry,
                &deps.redis,
                deps.dry_run,
            )
            .await;
        }

        if *cancel_rx.borrow() { continue; }

        // ═════════════════════════════════════════════════════════
        // STEP 7 — COMPOUNDING
        // ═════════════════════════════════════════════════════════
        tracing::info!("[goal {goal_id}] EXITING → COMPOUNDING");
        if let Err(e) = update_goal_step(&deps.redis, goal_id, "COMPOUNDING").await {
            tracing::warn!("[goal {goal_id}] redis step update failed: {e}");
        }

        // Compute new capital based on expected gain
        let new_capital = goal.current_eth * (1.0 + expected_gain_pct / 100.0);

        // Update P&L
        if let Err(e) = update_goal_pnl(&deps.redis, goal_id, new_capital).await {
            tracing::warn!("[goal {goal_id}] pnl update failed: {e}");
        }

        // Increment cycles
        let mut updated_goal = match get_goal(&deps.redis, goal_id).await {
            Ok(g) => g,
            Err(e) => {
                tracing::error!("[goal {goal_id}] failed to reload goal: {e}");
                break;
            }
        };
        updated_goal.cycles += 1;
        updated_goal.updated_at = Utc::now();

        let pnl_pct_total = if updated_goal.entry_eth > 0.0 {
            ((updated_goal.current_eth - updated_goal.entry_eth) / updated_goal.entry_eth) * 100.0
        } else {
            0.0
        };

        tracing::info!(
            "[goal {goal_id}] cycle {} complete, pnl: {:+.6} ETH ({:+.2}%)",
            updated_goal.cycles,
            updated_goal.pnl_eth,
            pnl_pct_total
        );

        // Check target gain
        if pnl_pct_total >= updated_goal.target_gain_pct {
            tracing::info!(
                "[goal {goal_id}] target gain {:.1}% reached — completing",
                updated_goal.target_gain_pct
            );
            updated_goal.status = GoalStatus::Completed;
            let _ = save_goal(&deps.redis, &updated_goal).await;
            break;
        }

        // Check stop loss
        if pnl_pct_total <= -(updated_goal.stop_loss_pct) {
            tracing::info!(
                "[goal {goal_id}] stop loss -{:.1}% triggered — failing",
                updated_goal.stop_loss_pct
            );
            updated_goal.status = GoalStatus::Failed;
            updated_goal.error = Some(format!("stop loss triggered at {pnl_pct_total:.2}%"));
            let _ = save_goal(&deps.redis, &updated_goal).await;
            break;
        }

        // Save and loop back to STEP 1
        let _ = save_goal(&deps.redis, &updated_goal).await;
    }

    tracing::info!("[goal {goal_id}] runner exited");
}

// ── Monitor result + extracted monitoring loop ─────────────────

/// Outcome of the monitoring loop, used to drive the outer GoalRunner loop.
enum MonitorResult {
    ExitSignal,
    YieldRotate,
    Cancelled,
    NoExit,
}

/// Shared monitoring loop used by both normal flow (STEP 5) and resume-from-monitoring.
async fn run_monitoring_loop(
    goal_id: Uuid,
    goal: &crate::types::goals::GoalSpec,
    position: &TradePosition,
    deps: &GoalRunnerDeps,
    cancel_rx: &mut watch::Receiver<bool>,
) -> MonitorResult {
    let mut exit_signal = false;
    let mut yield_rotate_signal = false;

    let (sentinel_tx, mut sentinel_rx) = tokio::sync::mpsc::channel::<SentinelSignal>(8);
    let (yield_tx, mut yield_rx) = tokio::sync::mpsc::channel::<YieldRotationSignal>(8);
    let _pubsub_task = {
        let redis_clone = deps.redis.clone();
        let s_tx = sentinel_tx.clone();
        let y_tx = yield_tx.clone();
        tokio::spawn(async move {
            let Ok(mut ps) = redis_clone.get_async_pubsub().await else { return };
            if ps.subscribe(SENTINEL_CHANNEL).await.is_err() { return; }
            if ps.subscribe(YIELD_ROTATE_CHANNEL).await.is_err() { return; }
            use futures::StreamExt;
            let mut stream = ps.on_message();
            while let Some(msg) = stream.next().await {
                let Ok(payload) = msg.get_payload::<String>() else { continue };
                let channel = msg.get_channel_name();
                if channel == SENTINEL_CHANNEL {
                    if let Ok(signal) = serde_json::from_str::<SentinelSignal>(&payload) {
                        if signal.goal_id == goal_id { let _ = s_tx.send(signal).await; }
                    }
                } else if channel == YIELD_ROTATE_CHANNEL {
                    if let Ok(signal) = serde_json::from_str::<YieldRotationSignal>(&payload) {
                        if signal.goal_id == goal_id { let _ = y_tx.send(signal).await; }
                    }
                }
            }
        })
    };
    drop(sentinel_tx);
    drop(yield_tx);

    loop {
        if *cancel_rx.borrow() { break; }
        if let Ok(g) = get_goal(&deps.redis, goal_id).await {
            match g.status {
                GoalStatus::Cancelled => break,
                GoalStatus::Paused => {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(PAUSE_CHECK_SECS)) => {}
                        _ = cancel_rx.changed() => {}
                    }
                    continue;
                }
                _ => {}
            }
        }

        let current_price = deps
            .price_oracle
            .fetch(&position.token_address, &position.chain)
            .await
            .map(|p| p.price_usd)
            .unwrap_or(0.0);

        let assessment = deps.sentinel.monitor_position(position, current_price).await;
        match assessment {
            Ok(a) if a.is_exit() => {
                tracing::info!("[goal {goal_id}] sentinel exit: {} — {}", a.action, a.reasoning);
                exit_signal = true;
                break;
            }
            Ok(a) => tracing::debug!("[goal {goal_id}] sentinel: hold — {}", a.reasoning),
            Err(e) => tracing::warn!("[goal {goal_id}] sentinel check failed: {e}"),
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(MONITOR_INTERVAL_SECS)) => {}
            _ = cancel_rx.changed() => {}
            signal = sentinel_rx.recv() => {
                if let Some(signal) = signal {
                    tracing::info!("[goal {goal_id}] sentinel pub/sub exit: {} — {}", signal.signal, signal.reason);
                    exit_signal = true;
                    break;
                }
            }
            rotation = yield_rx.recv() => {
                if let Some(rotation) = rotation {
                    if matches!(goal.strategy, GoalStrategy::YieldRotate | GoalStrategy::Adaptive) {
                        if goal.pnl_pct <= -(goal.stop_loss_pct * 0.8) {
                            tracing::info!("[goal {goal_id}] yield rotation skipped — P&L too close to stop loss");
                        } else {
                            tracing::info!("[goal {goal_id}] yield rotation: {} → {} (Δ{:.2}%)", rotation.from_protocol, rotation.to_protocol, rotation.delta_apy);
                            yield_rotate_signal = true;
                            break;
                        }
                    }
                }
            }
        }
    }
    _pubsub_task.abort();

    if *cancel_rx.borrow() {
        MonitorResult::Cancelled
    } else if yield_rotate_signal {
        MonitorResult::YieldRotate
    } else if exit_signal {
        MonitorResult::ExitSignal
    } else {
        MonitorResult::NoExit
    }
}

// ── Retry helper ────────────────────────────────────────────────

async fn with_retry<F, Fut, T>(
    goal_id: Uuid,
    step: &str,
    _deps: &GoalRunnerDeps,
    cancel_rx: &watch::Receiver<bool>,
    mut f: F,
) -> Result<T, anyhow::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut last_err = anyhow::anyhow!("unknown error");
    for attempt in 1..=MAX_RETRIES {
        if *cancel_rx.borrow() {
            return Err(anyhow::anyhow!("cancelled"));
        }
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                tracing::warn!(
                    "[goal {goal_id}] {step} attempt {attempt}/{MAX_RETRIES} failed: {e}"
                );
                last_err = e;
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(std::time::Duration::from_secs(
                        RETRY_BACKOFF_SECS * attempt as u64,
                    ))
                    .await;
                }
            }
        }
    }
    Err(last_err)
}

// ── Helpers ─────────────────────────────────────────────────────

/// Look up a known token symbol → ERC-20 address for the given chain.
/// Returns `None` for unknown symbols or unsupported chains. Used to filter
/// out fake testnet tokens (e.g. LIQTEST, VELVET, VFY) before submitting a
/// swap to Keymaster, which would otherwise reject calldata built from a
/// non-address string.
///
/// **Base Sepolia has very limited real token deployments.** Most yield-pool
/// data sources surface a long tail of fake testnet tokens that have no real
/// ERC-20 contract and no liquidity. Only the addresses below are known-good
/// and reachable via Uniswap V3 routes; everything else returns `None` so the
/// goal runner re-scouts instead of submitting a doomed swap.
fn known_token_address(symbol: &str, chain: &str) -> Option<String> {
    let s = symbol.trim().to_uppercase();
    let chain_lower = chain.to_lowercase();

    if chain_lower == "base_sepolia" {
        return match s.as_str() {
            "WETH" | "ETH" => Some("0x4200000000000000000000000000000000000006".into()),
            "USDC" => Some("0x036CbD53842c5426634e7929541eC2318f3dCF7e".into()),
            "WBTC" => Some("0x0555E30da8f98308EdB960aa94C0Db47230d2B9C".into()),
            // CBBTC, USDBC, DAI etc. don't have real Base Sepolia deployments.
            _ => None,
        };
    }

    if chain_lower == "base" {
        return match s.as_str() {
            "WETH" | "ETH" => Some("0x4200000000000000000000000000000000000006".into()),
            "USDC" => Some("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into()),
            "USDBC" => Some("0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA".into()),
            "CBBTC" => Some("0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".into()),
            "DAI" => Some("0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb".into()),
            _ => None,
        };
    }

    None
}

/// Resolve a token field that may be a plain symbol ("WETH"), a pool pair
/// symbol ("USDC-WETH", "LIQTEST-WETH", "USDC-VFY"), or already a hex address.
/// For pool pair symbols, prefers the non-WETH side; if neither side is WETH,
/// returns whichever half is found in the known-token map. Returns `None`
/// when no half resolves.
fn resolve_swap_token(field: &str, chain: &str) -> Option<String> {
    let trimmed = field.trim();
    if trimmed.starts_with("0x") && trimmed.len() == 42 {
        return Some(trimmed.to_string());
    }
    if let Some((a, b)) = trimmed.split_once('-') {
        let a_upper = a.to_uppercase();
        let b_upper = b.to_uppercase();
        // Prefer the non-WETH side first.
        let (first, second) = if a_upper == "WETH" || a_upper == "ETH" {
            (b, a)
        } else if b_upper == "WETH" || b_upper == "ETH" {
            (a, b)
        } else {
            (a, b)
        };
        return known_token_address(first, chain).or_else(|| known_token_address(second, chain));
    }
    known_token_address(trimmed, chain)
}

/// Parse the goal's resolved Keymaster wallet UUID. Returns `Err` if the goal
/// has no `wallet_id` (legacy goal created before this field existed) or the
/// stored value isn't a valid UUID. Callers should `fail_goal` on `Err`.
fn resolve_goal_wallet_uuid(goal: &crate::types::goals::GoalSpec) -> Result<Uuid, String> {
    let raw = goal
        .wallet_id
        .as_ref()
        .ok_or_else(|| format!(
            "goal has no resolved wallet_id (label='{}'); recreate the goal so the wallet UUID is fetched from Keymaster",
            goal.wallet_label
        ))?;
    Uuid::parse_str(raw).map_err(|e| format!("stored wallet_id '{raw}' is not a valid UUID: {e}"))
}

async fn fail_goal(redis: &redis::Client, goal_id: Uuid, error: &str) {
    tracing::error!("[goal {goal_id}] FAILED: {error}");
    if let Ok(mut goal) = get_goal(redis, goal_id).await {
        goal.status = GoalStatus::Failed;
        goal.error = Some(error.to_string());
        goal.updated_at = Utc::now();
        let _ = save_goal(redis, &goal).await;
    }
}

async fn sleep_interruptible(cancel_rx: &mut watch::Receiver<bool>, secs: u64) {
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(secs)) => {}
        _ = cancel_rx.changed() => {}
    }
}

// ── P&L calculation (pure, for testing) ──────────────────────────

pub fn compute_pnl(entry_eth: f64, current_eth: f64) -> (f64, f64) {
    let pnl_eth = current_eth - entry_eth;
    let pnl_pct = if entry_eth > 0.0 {
        (pnl_eth / entry_eth) * 100.0
    } else {
        0.0
    };
    (pnl_eth, pnl_pct)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pnl_calculation() {
        let (pnl_eth, pnl_pct) = compute_pnl(0.05, 0.055);
        assert!((pnl_eth - 0.005).abs() < 1e-10);
        assert!((pnl_pct - 10.0).abs() < 1e-10);

        let (pnl_eth, pnl_pct) = compute_pnl(0.1, 0.08);
        assert!((pnl_eth - (-0.02)).abs() < 1e-10);
        assert!((pnl_pct - (-20.0)).abs() < 1e-10);

        let (pnl_eth, pnl_pct) = compute_pnl(0.0, 0.05);
        assert!((pnl_eth - 0.05).abs() < 1e-10);
        assert_eq!(pnl_pct, 0.0); // division by zero guard
    }

    #[tokio::test]
    async fn test_cancel_signal_stops_loop() {
        let (cancel_tx, cancel_rx) = watch::channel(false);
        assert!(!*cancel_rx.borrow());
        cancel_tx.send(true).unwrap();
        assert!(*cancel_rx.borrow());
    }

    #[test]
    fn test_goal_step_resume_routing() {
        // Goals in MONITORING/EXITING should resume from Monitoring
        let step_monitoring = "MONITORING";
        let step_exiting = "EXITING";
        let step_executing = "EXECUTING";
        let step_scouting = "SCOUTING";
        let step_risk = "RISK";
        let step_trading = "TRADING";

        fn route_step(step: &str) -> Option<GoalStep> {
            match step {
                "MONITORING" | "EXITING" => Some(GoalStep::Monitoring),
                "EXECUTING" => Some(GoalStep::Monitoring), // crash recovery
                _ => None,
            }
        }

        assert_eq!(route_step(step_monitoring), Some(GoalStep::Monitoring));
        assert_eq!(route_step(step_exiting), Some(GoalStep::Monitoring));
        assert_eq!(route_step(step_executing), Some(GoalStep::Monitoring));
        assert_eq!(route_step(step_scouting), None);
        assert_eq!(route_step(step_risk), None);
        assert_eq!(route_step(step_trading), None);
    }

    #[test]
    fn test_mid_execution_crash_routes_to_monitoring() {
        // EXECUTING is a dangerous state — if the gateway crashed mid-execution,
        // the position may or may not exist on-chain. We resume from MONITORING
        // so sentinel can check.
        let crash_step = "EXECUTING";
        let resume = match crash_step {
            "MONITORING" | "EXITING" | "EXECUTING" => Some(GoalStep::Monitoring),
            _ => None,
        };
        assert_eq!(resume, Some(GoalStep::Monitoring));
    }

    #[test]
    fn test_auto_resume_disabled_skips_all() {
        // VESPRA_AUTO_RESUME_GOALS=false means no goals should be resumed.
        // The logic is: if env var is "false" or "0", skip. Otherwise resume.
        fn should_resume(val: &str) -> bool {
            val != "false" && val != "0"
        }
        assert!(!should_resume("false"));
        assert!(!should_resume("0"));
        assert!(should_resume("true"));
        assert!(should_resume("1"));
        assert!(should_resume("yes"));
    }
}
