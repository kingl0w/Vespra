use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agents::sentinel::SentinelAgent;
use crate::chain::ChainRegistry;
use crate::data::price::PriceOracle;
use crate::routes::goals::{get_goal, list_goals_by_status, save_goal};
use crate::types::goals::GoalStatus;

//── constants ──────────────────────────────────────────────────

const DEFAULT_INTERVAL_SECS: u64 = 300;

///redis pub/sub channel for exit signals.
pub const SENTINEL_CHANNEL: &str = "vespra:sentinel:signals";

///redis key prefix for daily signal counter.
const SIGNALS_COUNT_PREFIX: &str = "sentinel:signals:count";

//── public types ───────────────────────────────────────────────

///signal published to redis pub/sub when sentinel decides to exit a position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelSignal {
    pub goal_id: Uuid,
    pub signal: String,
    pub reason: String,
    pub current_price: f64,
}

///snapshot of the monitor's runtime status (served by get /sentinel/status).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelStatus {
    pub running: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub goals_monitored: u32,
    pub signals_sent_today: u32,
}

///shared handle so the route handler can read live status.
#[derive(Clone)]
pub struct SentinelMonitor {
    pub status: Arc<RwLock<SentinelStatus>>,
    pub active_goals: Arc<RwLock<HashSet<Uuid>>>,
}

impl SentinelMonitor {
    pub fn new() -> Self {
        Self {
            status: Arc::new(RwLock::new(SentinelStatus {
                running: false,
                last_run: None,
                goals_monitored: 0,
                signals_sent_today: 0,
            })),
            active_goals: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    ///long-running background task. spawned once at gateway boot.
    pub async fn run(
        monitor: Arc<SentinelMonitor>,
        redis: Arc<redis::Client>,
        sentinel: Arc<SentinelAgent>,
        price_oracle: Arc<dyn PriceOracle>,
        chain_registry: Arc<ChainRegistry>,
        telegram: Option<crate::notifications::TelegramClient>,
    ) {
        let interval_secs: u64 = std::env::var("SENTINEL_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_INTERVAL_SECS);

        tracing::info!(
            "[sentinel] monitor started — polling every {interval_secs}s"
        );

        {
            let mut s = monitor.status.write().await;
            s.running = true;
        }

        loop {
            let tick_result = Self::tick(
                &monitor,
                &redis,
                &sentinel,
                &price_oracle,
                &chain_registry,
                &telegram,
            )
            .await;

            if let Err(e) = tick_result {
                tracing::error!("[sentinel] tick failed: {e}");
            }

            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }

    ///single polling iteration: load running goals, check each, publish signals.
    async fn tick(
        monitor: &Arc<SentinelMonitor>,
        redis: &Arc<redis::Client>,
        sentinel: &Arc<SentinelAgent>,
        price_oracle: &Arc<dyn PriceOracle>,
        chain_registry: &Arc<ChainRegistry>,
        telegram: &Option<crate::notifications::TelegramClient>,
    ) -> anyhow::Result<()> {
        use redis::AsyncCommands;

        let goals = list_goals_by_status(redis, GoalStatus::Running).await?;
        let goal_count = goals.len() as u32;
        let mut signal_count: u32 = 0;
        let today = Utc::now().format("%Y-%m-%d").to_string();

        monitor.active_goals.write().await.clear();

        for goal in &goals {
            //skip goals not in monitoring step
            if goal.current_step != "MONITORING" {
                continue;
            }

            //ves-115: dedupe — if this goal is already being monitored by an
            //in-flight sentinel evaluation, skip the duplicate spawn.
            {
                let mut active = monitor.active_goals.write().await;
                if active.contains(&goal.id) {
                    tracing::debug!(
                        "sentinel already active for goal {} — skipping duplicate spawn",
                        goal.id
                    );
                    continue;
                }
                active.insert(goal.id);
            }

            //ves-94: testnet chains have no real price feeds — defillama
            //returns 0 for sepolia tokens. on testnet, use time-based
            //completion instead of price evaluation.
            let chain_lc = goal.chain.to_lowercase();
            if chain_lc.contains("sepolia") || chain_lc.contains("testnet") {
                let timeout_mins: u64 = std::env::var("VESPRA_TESTNET_MONITOR_TIMEOUT_MINUTES")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(5);
                let elapsed = Utc::now().signed_duration_since(goal.updated_at);
                let elapsed_mins = elapsed.num_seconds().max(0) as u64 / 60;

                if elapsed_mins >= timeout_mins {
                    tracing::info!(
                        "[sentinel] goal {} testnet monitoring period complete ({elapsed_mins}m >= {timeout_mins}m) — completing",
                        goal.id
                    );
                    complete_goal_inline(
                        redis,
                        goal.id,
                        "testnet goal completed after monitoring period",
                    )
                    .await;
                } else {
                    tracing::info!(
                        "[sentinel] goal {} testnet chain detected, skipping price evaluation ({elapsed_mins}m / {timeout_mins}m)",
                        goal.id
                    );
                }
                continue;
            }

            //ves-fix: use the goal's tracked token address; fall back to the
            //chain's native (wrapped) token if the goal hasn't recorded one yet.
            //previously this passed goal.id.to_string() (a uuid) which the
            //defillama oracle treated as a contract address and always returned 0.
            let token_addr = goal
                .token_address
                .clone()
                .or_else(|| {
                    chain_registry
                        .get(&goal.chain)
                        .map(|c| c.native_token_address.clone())
                })
                .unwrap_or_default();

            let current_price = if token_addr.is_empty() {
                0.0
            } else {
                price_oracle
                    .fetch(&token_addr, &goal.chain)
                    .await
                    .map(|p| p.price_usd)
                    .unwrap_or(0.0)
            };

            //ves-93: validate current_price before any sentinel comparison.
            //nan/inf/<=0 disables every downstream gain/loss check silently.
            if current_price.is_nan() || current_price.is_infinite() || current_price <= 0.0 {
                tracing::warn!(
                    "[sentinel] goal {} invalid current_price ({}) — failing goal",
                    goal.id, current_price
                );
                fail_goal_inline(
                    redis,
                    goal.id,
                    &format!(
                        "invalid current_price ({}) — sentinel cannot evaluate, goal aborted",
                        current_price
                    ),
                )
                .await;
                continue;
            }

            let entry_price_derived = if goal.entry_eth > 0.0 && goal.pnl_pct != 0.0 {
                current_price / (1.0 + goal.pnl_pct / 100.0)
            } else {
                current_price
            };
            if entry_price_derived.is_nan()
                || entry_price_derived.is_infinite()
                || entry_price_derived <= 0.0
            {
                tracing::warn!(
                    "[sentinel] goal {} invalid entry_price ({}) — failing goal",
                    goal.id, entry_price_derived
                );
                fail_goal_inline(
                    redis,
                    goal.id,
                    &format!(
                        "invalid entry_price ({}) — sentinel cannot evaluate, goal aborted",
                        entry_price_derived
                    ),
                )
                .await;
                continue;
            }

            //build a lightweight tradeposition for sentinel assessment
            let position = crate::types::trade_up::TradePosition {
                id: goal.id.to_string(),
                wallet: goal.wallet_label.clone(),
                chain: goal.chain.clone(),
                token_address: goal.id.to_string(),
                token_symbol: String::new(),
                entry_price_usd: entry_price_derived,
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

            let assessment = sentinel.monitor_position(&position, current_price).await;

            let signal_type = match &assessment {
                Ok(a) if a.action == "exit_loss" => Some("EXIT_STOP_LOSS"),
                Ok(a) if a.action == "exit_gain" => Some("EXIT_TARGET_HIT"),
                Ok(a) if a.is_exit() => Some("EXIT_ANOMALY"),
                _ => None,
            };

            if let (Some(sig), Ok(a)) = (signal_type, &assessment) {
                let signal = SentinelSignal {
                    goal_id: goal.id,
                    signal: sig.to_string(),
                    reason: a.reasoning.clone(),
                    current_price,
                };

                let payload = serde_json::to_string(&signal)?;

                //publish to redis pub/sub
                let mut conn = redis::Client::get_multiplexed_async_connection(redis.as_ref()).await?;
                let _: i64 = redis::cmd("PUBLISH")
                    .arg(SENTINEL_CHANNEL)
                    .arg(&payload)
                    .query_async(&mut conn)
                    .await?;

                //increment daily counter
                let count_key = format!("{SIGNALS_COUNT_PREFIX}:{today}");
                let _: () = conn.incr(&count_key, 1).await?;
                //expire after 48h so keys auto-clean
                let _: () = conn.expire(&count_key, 172800).await?;

                signal_count += 1;

                //ves-118: exits are an expected outcome, not a warning. log
                //at info so warn-level filters surface only real failures.
                tracing::info!(
                    "[sentinel] EXIT signal for goal {}: {} — {}",
                    goal.id,
                    sig,
                    a.reasoning
                );

                if let Some(tg) = telegram {
                    let tg = tg.clone();
                    let reason = crate::notifications::escape_markdown(&a.reasoning);
                    let msg = format!(
                        "Sentinel exit signal \u{2014} goal {}, reason: {}, price: {:.4}",
                        goal.id, reason, current_price
                    );
                    tokio::spawn(async move {
                        let _ = tg.send(&msg).await;
                    });
                }
            } else if let Ok(a) = &assessment {
                tracing::debug!(
                    "[sentinel] goal {} → HOLD — {}",
                    goal.id,
                    a.reasoning
                );
            } else if let Err(e) = &assessment {
                tracing::warn!(
                    "[sentinel] goal {} check failed (non-fatal): {e}",
                    goal.id
                );
            }
        }

        tracing::info!(
            "[sentinel] checked {goal_count} goals, {signal_count} exit signals published"
        );

        //update shared status
        let mut conn = redis::Client::get_multiplexed_async_connection(redis.as_ref()).await?;
        let today_key = format!("{SIGNALS_COUNT_PREFIX}:{today}");
        let total_today: u32 = redis::AsyncCommands::get(&mut conn, &today_key)
            .await
            .unwrap_or(0);

        {
            let mut s = monitor.status.write().await;
            s.last_run = Some(Utc::now());
            s.goals_monitored = goal_count;
            s.signals_sent_today = total_today;
        }

        Ok(())
    }
}

//── helpers ────────────────────────────────────────────────────

async fn complete_goal_inline(redis: &Arc<redis::Client>, goal_id: Uuid, reason: &str) {
    tracing::info!("[sentinel] goal {goal_id} COMPLETED: {reason}");
    if let Ok(mut goal) = get_goal(redis, goal_id).await {
        goal.status = GoalStatus::Completed;
        goal.error = Some(reason.to_string());
        goal.updated_at = Utc::now();
        if let Err(e) = save_goal(redis, &goal).await {
            tracing::warn!("[sentinel] failed to persist Completed status for goal {goal_id}: {e}");
        }
    }
}

async fn fail_goal_inline(redis: &Arc<redis::Client>, goal_id: Uuid, error: &str) {
    tracing::error!("[sentinel] goal {goal_id} FAILED: {error}");
    if let Ok(mut goal) = get_goal(redis, goal_id).await {
        goal.status = GoalStatus::Failed;
        goal.error = Some(error.to_string());
        goal.failed_at_step = Some(goal.current_step.clone());
        goal.updated_at = Utc::now();
        if let Err(e) = save_goal(redis, &goal).await {
            tracing::warn!("[sentinel] failed to persist Failed status for goal {goal_id}: {e}");
        }
    }
}

//── tests ──────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn signal_serialization_roundtrip() {
        let signal = SentinelSignal {
            goal_id: Uuid::parse_str("a1a2a3a4-b1b2-c1c2-d1d2-e1e2e3e4e5e6").unwrap(),
            signal: "EXIT_STOP_LOSS".to_string(),
            reason: "price dropped 6.2% below entry, exceeding 5% stop loss".to_string(),
            current_price: 1823.45,
        };

        let json = serde_json::to_string(&signal).unwrap();
        assert!(json.contains("EXIT_STOP_LOSS"));
        assert!(json.contains("a1a2a3a4"));
        assert!(json.contains("1823.45"));

        let parsed: SentinelSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.goal_id, signal.goal_id);
        assert_eq!(parsed.signal, "EXIT_STOP_LOSS");
        assert!((parsed.current_price - 1823.45).abs() < f64::EPSILON);
    }

    #[test]
    fn exit_stop_loss_identified_correctly() {
        //simulate: entry at $2000, current at $1880 → -6% (exceeds 5% stop loss)
        let entry_price = 2000.0;
        let current_price = 1880.0;
        let stop_loss_pct = 5.0;

        let loss_pct = ((entry_price - current_price) / entry_price) * 100.0;
        assert!(loss_pct > stop_loss_pct, "loss {loss_pct}% should exceed stop {stop_loss_pct}%");

        //the signal type that would be assigned
        let signal = if loss_pct > stop_loss_pct {
            "EXIT_STOP_LOSS"
        } else {
            "HOLD"
        };
        assert_eq!(signal, "EXIT_STOP_LOSS");
    }

    #[test]
    fn exit_target_hit_identified_correctly() {
        //simulate: entry at $2000, current at $2250 → +12.5% (exceeds 10% target)
        let entry_price = 2000.0;
        let current_price = 2250.0;
        let target_gain_pct = 10.0;

        let gain_pct = ((current_price - entry_price) / entry_price) * 100.0;
        assert!(gain_pct > target_gain_pct);

        let signal = if gain_pct > target_gain_pct {
            "EXIT_TARGET_HIT"
        } else {
            "HOLD"
        };
        assert_eq!(signal, "EXIT_TARGET_HIT");
    }

    #[test]
    fn hold_when_within_thresholds() {
        let entry_price = 2000.0;
        let current_price = 1970.0; // -1.5%, within 5% stop loss
        let stop_loss_pct = 5.0;
        let target_gain_pct = 10.0;

        let change_pct = ((current_price - entry_price) / entry_price) * 100.0;
        let is_exit = change_pct <= -(stop_loss_pct) || change_pct >= target_gain_pct;
        assert!(!is_exit, "should HOLD when within thresholds");
    }
}
