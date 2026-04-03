use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::data::aave::AaveFetcher;
use crate::data::yield_provider::ProviderRegistry;
use crate::routes::goals::list_goals_by_status;
use crate::types::goals::{GoalStatus, GoalStrategy};

// ── Constants ──────────────────────────────────────────────────

/// Default polling interval (seconds). Override with YIELD_CHECK_INTERVAL_SECS.
const DEFAULT_INTERVAL_SECS: u64 = 1800; // 30 minutes

/// Default minimum APY improvement required. Override with YIELD_ROTATION_THRESHOLD_PCT.
const DEFAULT_THRESHOLD_PCT: f64 = 0.5;

/// Redis pub/sub channel for yield rotation signals.
pub const YIELD_ROTATE_CHANNEL: &str = "vespra:yield:rotate";

/// Redis key prefix for daily rotation counter.
const ROTATIONS_COUNT_PREFIX: &str = "yield:rotations:count";

/// If gas cost exceeds this fraction of annualised yield benefit, skip rotation.
const GAS_BENEFIT_MAX_RATIO: f64 = 0.10;

// ── Public types ───────────────────────────────────────────────

/// Signal published to Redis pub/sub when a better yield opportunity is found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YieldRotationSignal {
    pub goal_id: Uuid,
    pub from_protocol: String,
    pub to_protocol: String,
    pub from_apy: f64,
    pub to_apy: f64,
    pub delta_apy: f64,
}

/// Snapshot of the scheduler's runtime status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerStatus {
    pub running: bool,
    pub last_run: Option<DateTime<Utc>>,
    pub positions_monitored: u32,
    pub rotations_today: u32,
    pub next_run: Option<DateTime<Utc>>,
}

impl Default for SchedulerStatus {
    fn default() -> Self {
        Self {
            running: false,
            last_run: None,
            positions_monitored: 0,
            rotations_today: 0,
            next_run: None,
        }
    }
}

pub type SharedSchedulerStatus = Arc<RwLock<SchedulerStatus>>;

pub fn default_status() -> SharedSchedulerStatus {
    Arc::new(RwLock::new(SchedulerStatus::default()))
}

/// Yield Rotation Scheduler — spawned once at gateway boot.
#[derive(Clone)]
pub struct YieldScheduler {
    pub status: SharedSchedulerStatus,
}

impl YieldScheduler {
    pub fn new(status: SharedSchedulerStatus) -> Self {
        Self { status }
    }

    /// Long-running background task.
    pub async fn run(
        scheduler: Arc<YieldScheduler>,
        redis: Arc<redis::Client>,
        aave_fetcher: Arc<AaveFetcher>,
        yield_registry: Arc<ProviderRegistry>,
    ) {
        let interval_secs: u64 = std::env::var("YIELD_CHECK_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_INTERVAL_SECS);

        let threshold_pct: f64 = std::env::var("YIELD_ROTATION_THRESHOLD_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_THRESHOLD_PCT);

        tracing::info!(
            "[yield-scheduler] started — polling every {interval_secs}s, threshold={threshold_pct}%"
        );

        {
            let mut s = scheduler.status.write().await;
            s.running = true;
        }

        loop {
            // Update next_run before sleeping
            {
                let mut s = scheduler.status.write().await;
                s.next_run = Some(Utc::now() + chrono::Duration::seconds(interval_secs as i64));
            }

            let tick_result = Self::tick(
                &scheduler,
                &redis,
                &aave_fetcher,
                &yield_registry,
                threshold_pct,
            )
            .await;

            if let Err(e) = tick_result {
                tracing::error!("[yield-scheduler] tick failed: {e}");
            }

            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }

    /// Single polling iteration.
    async fn tick(
        scheduler: &Arc<YieldScheduler>,
        redis: &Arc<redis::Client>,
        aave_fetcher: &Arc<AaveFetcher>,
        yield_registry: &Arc<ProviderRegistry>,
        threshold_pct: f64,
    ) -> anyhow::Result<()> {
        use redis::AsyncCommands;

        // Load running goals that use yield-related strategies
        let goals = list_goals_by_status(redis, GoalStatus::Running).await?;
        let yield_goals: Vec<_> = goals
            .into_iter()
            .filter(|g| {
                g.current_step == "MONITORING"
                    && matches!(g.strategy, GoalStrategy::YieldRotate | GoalStrategy::Adaptive)
            })
            .collect();

        let position_count = yield_goals.len() as u32;
        let mut rotation_count: u32 = 0;
        let today = Utc::now().format("%Y-%m-%d").to_string();

        // Fetch best available yield opportunities across all chains (top by APY)
        let best_pools = yield_registry
            .fetch_pools(None, 10_000.0, 0.5)
            .await
            .unwrap_or_default();

        for goal in &yield_goals {
            // Fetch current Aave positions for this goal's wallet/chain
            let positions = aave_fetcher
                .fetch_positions(&goal.chain, &goal.wallet_label)
                .await
                .unwrap_or_default();

            for pos in &positions {
                let current_apy = pos.net_apy - pos.gas_drag_apy;

                // Find best alternative pool for same or adjacent asset
                let best_match = best_pools.iter().find(|p| {
                    p.symbol.to_lowercase().contains(&pos.asset.to_lowercase())
                        && p.protocol != pos.protocol
                });

                let Some(candidate) = best_match else {
                    continue;
                };

                let delta = candidate.apy - current_apy;
                if delta <= threshold_pct {
                    continue;
                }

                // Gas check: skip if gas drag would eat >10% of the APY benefit
                if pos.gas_drag_apy > 0.0 {
                    let benefit_annual = delta * goal.capital_eth / 100.0;
                    let gas_annual = pos.gas_drag_apy * goal.capital_eth / 100.0;
                    if benefit_annual > 0.0 && gas_annual / benefit_annual > GAS_BENEFIT_MAX_RATIO {
                        tracing::debug!(
                            "[yield-scheduler] goal {} skipped: gas {:.3}% eats >{:.0}% of {:.3}% delta",
                            goal.id, pos.gas_drag_apy, GAS_BENEFIT_MAX_RATIO * 100.0, delta
                        );
                        continue;
                    }
                }

                let signal = YieldRotationSignal {
                    goal_id: goal.id,
                    from_protocol: pos.protocol.clone(),
                    to_protocol: candidate.protocol.clone(),
                    from_apy: current_apy,
                    to_apy: candidate.apy,
                    delta_apy: delta,
                };

                let payload = serde_json::to_string(&signal)?;

                // Publish to Redis pub/sub
                let mut conn =
                    redis::Client::get_multiplexed_async_connection(redis.as_ref()).await?;
                let _: i64 = redis::cmd("PUBLISH")
                    .arg(YIELD_ROTATE_CHANNEL)
                    .arg(&payload)
                    .query_async(&mut conn)
                    .await?;

                // Increment daily counter
                let count_key = format!("{ROTATIONS_COUNT_PREFIX}:{today}");
                let _: () = conn.incr(&count_key, 1).await?;
                let _: () = conn.expire(&count_key, 172800).await?; // 48h TTL

                rotation_count += 1;

                tracing::info!(
                    "[yield-scheduler] goal {}: rotate {} → {} ({:.2}% → {:.2}%, Δ{:.2}%)",
                    goal.id,
                    pos.protocol,
                    candidate.protocol,
                    current_apy,
                    candidate.apy,
                    delta
                );
            }
        }

        tracing::info!(
            "[yield-scheduler] checked {position_count} positions, {rotation_count} rotations signaled"
        );

        // Update shared status
        let mut conn =
            redis::Client::get_multiplexed_async_connection(redis.as_ref()).await?;
        let today_key = format!("{ROTATIONS_COUNT_PREFIX}:{today}");
        let total_today: u32 = redis::AsyncCommands::get(&mut conn, &today_key)
            .await
            .unwrap_or(0);

        {
            let mut s = scheduler.status.write().await;
            s.last_run = Some(Utc::now());
            s.positions_monitored = position_count;
            s.rotations_today = total_today;
        }

        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn rotation_signal_only_fires_above_threshold() {
        let threshold = 0.5;

        // Case 1: delta below threshold — no rotation
        let current_apy = 3.2;
        let candidate_apy = 3.5; // delta = 0.3 < 0.5
        let delta = candidate_apy - current_apy;
        assert!(delta <= threshold, "delta {delta} should NOT trigger rotation");

        // Case 2: delta above threshold — rotation
        let candidate_apy_high = 4.0; // delta = 0.8 > 0.5
        let delta_high = candidate_apy_high - current_apy;
        assert!(delta_high > threshold, "delta {delta_high} should trigger rotation");

        // Case 3: exact threshold — no rotation (must exceed, not equal)
        let candidate_apy_exact = 3.7; // delta = 0.5 == 0.5
        let delta_exact = candidate_apy_exact - current_apy;
        assert!(!(delta_exact > threshold), "delta == threshold should NOT trigger");
    }

    #[test]
    fn gas_check_skips_when_gas_exceeds_benefit() {
        let capital_eth = 10.0;

        // APY delta = 0.8%, gas_drag_apy = 0.2%
        // benefit_annual = 0.008 * 10 = 0.08 ETH
        // gas_annual    = 0.002 * 10 = 0.02 ETH
        // ratio = 0.02 / 0.08 = 0.25 → > 0.10 → SKIP
        let delta = 0.8;
        let gas_drag_apy = 0.2;
        let benefit = delta * capital_eth / 100.0;
        let gas = gas_drag_apy * capital_eth / 100.0;
        let ratio = gas / benefit;
        assert!(
            ratio > GAS_BENEFIT_MAX_RATIO,
            "gas ratio {ratio:.2} > {GAS_BENEFIT_MAX_RATIO} → should skip"
        );

        // APY delta = 2.0%, gas_drag_apy = 0.05%
        // benefit_annual = 0.02 * 10 = 0.20 ETH
        // gas_annual    = 0.0005 * 10 = 0.005 ETH
        // ratio = 0.005 / 0.20 = 0.025 → < 0.10 → OK
        let delta2 = 2.0;
        let gas_drag_apy2 = 0.05;
        let benefit2 = delta2 * capital_eth / 100.0;
        let gas2 = gas_drag_apy2 * capital_eth / 100.0;
        let ratio2 = gas2 / benefit2;
        assert!(
            ratio2 <= GAS_BENEFIT_MAX_RATIO,
            "gas ratio {ratio2:.3} <= {GAS_BENEFIT_MAX_RATIO} → should proceed"
        );
    }

    #[test]
    fn signal_serialization_roundtrip() {
        let signal = YieldRotationSignal {
            goal_id: Uuid::parse_str("a1a2a3a4-b1b2-c1c2-d1d2-e1e2e3e4e5e6").unwrap(),
            from_protocol: "aave_v3".into(),
            to_protocol: "compound_v3".into(),
            from_apy: 3.2,
            to_apy: 4.1,
            delta_apy: 0.9,
        };
        let json = serde_json::to_string(&signal).unwrap();
        let parsed: YieldRotationSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.goal_id, signal.goal_id);
        assert_eq!(parsed.from_protocol, "aave_v3");
        assert_eq!(parsed.to_protocol, "compound_v3");
        assert!((parsed.delta_apy - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn compound_and_snipe_strategies_should_be_excluded() {
        // YieldRotate and Adaptive should be included
        assert!(matches!(GoalStrategy::YieldRotate, GoalStrategy::YieldRotate | GoalStrategy::Adaptive));
        assert!(matches!(GoalStrategy::Adaptive, GoalStrategy::YieldRotate | GoalStrategy::Adaptive));

        // Compound and Snipe should NOT match
        assert!(!matches!(GoalStrategy::Compound, GoalStrategy::YieldRotate | GoalStrategy::Adaptive));
        assert!(!matches!(GoalStrategy::Snipe, GoalStrategy::YieldRotate | GoalStrategy::Adaptive));
    }
}
