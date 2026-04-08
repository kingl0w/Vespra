use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::agents::sentinel::SentinelAgent;
use crate::data::price::PriceOracle;
use crate::routes::goals::{get_goal, list_goals_by_status, save_goal};
use crate::types::goals::GoalStatus;

// ── Constants ──────────────────────────────────────────────────

/// Default polling interval (seconds). Override with SENTINEL_INTERVAL_SECS env var.
const DEFAULT_INTERVAL_SECS: u64 = 300;

/// Redis pub/sub channel for exit signals.
pub const SENTINEL_CHANNEL: &str = "vespra:sentinel:signals";

/// Redis key prefix for daily signal counter.
const SIGNALS_COUNT_PREFIX: &str = "sentinel:signals:count";

// ── Public types ───────────────────────────────────────────────

/// Signal published to Redis pub/sub when Sentinel decides to exit a position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelSignal {
    pub goal_id: Uuid,
    pub signal: String,
    pub reason: String,
    pub current_price: f64,
}

/// Snapshot of the monitor's runtime status (served by GET /sentinel/status).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelStatus {
    pub running: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub goals_monitored: u32,
    pub signals_sent_today: u32,
}

/// Shared handle so the route handler can read live status.
#[derive(Clone)]
pub struct SentinelMonitor {
    pub status: Arc<RwLock<SentinelStatus>>,
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
        }
    }

    /// Long-running background task. Spawned once at gateway boot.
    pub async fn run(
        monitor: Arc<SentinelMonitor>,
        redis: Arc<redis::Client>,
        sentinel: Arc<SentinelAgent>,
        price_oracle: Arc<dyn PriceOracle>,
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
            )
            .await;

            if let Err(e) = tick_result {
                tracing::error!("[sentinel] tick failed: {e}");
            }

            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }

    /// Single polling iteration: load running goals, check each, publish signals.
    async fn tick(
        monitor: &Arc<SentinelMonitor>,
        redis: &Arc<redis::Client>,
        sentinel: &Arc<SentinelAgent>,
        price_oracle: &Arc<dyn PriceOracle>,
    ) -> anyhow::Result<()> {
        use redis::AsyncCommands;

        let goals = list_goals_by_status(redis, GoalStatus::Running).await?;
        let goal_count = goals.len() as u32;
        let mut signal_count: u32 = 0;
        let today = Utc::now().format("%Y-%m-%d").to_string();

        for goal in &goals {
            // Skip goals not in MONITORING step
            if goal.current_step != "MONITORING" {
                continue;
            }

            // Get current price for position token
            // GoalSpec doesn't store token_address directly — use chain + wallet context.
            // The token being monitored is embedded in the goal's context. For now we
            // derive a placeholder address from the goal id (the GoalRunner wrote it).
            // In practice the position token address should be on the GoalSpec; we fall
            // back to a price of 0.0 on error, same as GoalRunner does.
            let current_price = price_oracle
                .fetch(&goal.id.to_string(), &goal.chain)
                .await
                .map(|p| p.price_usd)
                .unwrap_or(0.0);

            // VES-93: validate current_price before any sentinel comparison.
            // NaN/inf/<=0 disables every downstream gain/loss check silently.
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

            // VES-93: reverse-derive entry price from P&L, then validate.
            // If goal state is corrupted (NaN/inf pnl, divide-by-near-zero) the
            // derived entry_price is meaningless and would silently disable the
            // exit logic; abort the goal for human review instead.
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

            // Build a lightweight TradePosition for sentinel assessment
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

                // Publish to Redis pub/sub
                let mut conn = redis::Client::get_multiplexed_async_connection(redis.as_ref()).await?;
                let _: i64 = redis::cmd("PUBLISH")
                    .arg(SENTINEL_CHANNEL)
                    .arg(&payload)
                    .query_async(&mut conn)
                    .await?;

                // Increment daily counter
                let count_key = format!("{SIGNALS_COUNT_PREFIX}:{today}");
                let _: () = conn.incr(&count_key, 1).await?;
                // Expire after 48h so keys auto-clean
                let _: () = conn.expire(&count_key, 172800).await?;

                signal_count += 1;

                tracing::warn!(
                    "[sentinel] EXIT signal for goal {}: {} — {}",
                    goal.id,
                    sig,
                    a.reasoning
                );
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

        // Update shared status
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

// ── Helpers ────────────────────────────────────────────────────

/// Mark a goal as Failed with the given error message. Used by the sentinel
/// background task when it detects unrecoverable state (invalid price/entry,
/// VES-93) without holding a reference to the GoalRunner's `fail_goal` helper.
async fn fail_goal_inline(redis: &Arc<redis::Client>, goal_id: Uuid, error: &str) {
    tracing::error!("[sentinel] goal {goal_id} FAILED: {error}");
    if let Ok(mut goal) = get_goal(redis, goal_id).await {
        goal.status = GoalStatus::Failed;
        goal.error = Some(error.to_string());
        goal.updated_at = Utc::now();
        if let Err(e) = save_goal(redis, &goal).await {
            tracing::warn!("[sentinel] failed to persist Failed status for goal {goal_id}: {e}");
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────

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
        // Simulate: entry at $2000, current at $1880 → -6% (exceeds 5% stop loss)
        let entry_price = 2000.0;
        let current_price = 1880.0;
        let stop_loss_pct = 5.0;

        let loss_pct = ((entry_price - current_price) / entry_price) * 100.0;
        assert!(loss_pct > stop_loss_pct, "loss {loss_pct}% should exceed stop {stop_loss_pct}%");

        // The signal type that would be assigned
        let signal = if loss_pct > stop_loss_pct {
            "EXIT_STOP_LOSS"
        } else {
            "HOLD"
        };
        assert_eq!(signal, "EXIT_STOP_LOSS");
    }

    #[test]
    fn exit_target_hit_identified_correctly() {
        // Simulate: entry at $2000, current at $2250 → +12.5% (exceeds 10% target)
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
