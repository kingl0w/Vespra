
use chrono::Utc;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::executor::ExecutorAgent;
use crate::config::GatewayConfig;
use crate::chain::ChainRegistry;
use crate::execution_gate;
use crate::types::tx::TxStatus;

//── constants ──────────────────────────────────────────────────

const DEFAULT_FEE_RATE_BPS: u32 = 10;

///redis keys
const FEE_TOTAL_KEY: &str = "fees:total";
const FEE_TODAY_PREFIX: &str = "fees:daily";

///read the fee rate from env (once, at call time).
pub fn fee_rate_bps() -> u32 {
    std::env::var("FEE_RATE_BPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_FEE_RATE_BPS)
}

///read the treasury address from env.
pub fn treasury_address() -> String {
    std::env::var("VESPRA_TREASURY_ADDRESS").unwrap_or_default()
}

//── fee record ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRecord {
    pub goal_id: Uuid,
    pub fee_eth: f64,
    pub fee_rate_bps: u32,
    pub tx_hash: String,
    pub timestamp: String,
}

fn fee_key(goal_id: &Uuid) -> String {
    format!("fees:{goal_id}")
}

//── core fee logic ─────────────────────────────────────────────

///calculate the fee for a goal exit. returns 0.0 if there's no profit.
pub fn calculate_fee(entry_eth: f64, current_eth: f64, rate_bps: u32) -> f64 {
    let net_gain = current_eth - entry_eth;
    if net_gain <= 0.0 {
        return 0.0;
    }
    net_gain * (rate_bps as f64) / 10_000.0
}

pub async fn collect_exit_fee(
    goal_id: Uuid,
    entry_eth: f64,
    current_eth: f64,
    chain: &str,
    executor: &ExecutorAgent,
    config: &GatewayConfig,
    chain_registry: &ChainRegistry,
    http_client: &reqwest::Client,
    redis: &redis::Client,
    dry_run: bool,
    telegram: &Option<crate::notifications::TelegramClient>,
) {
    let rate = fee_rate_bps();
    let fee = calculate_fee(entry_eth, current_eth, rate);

    if fee <= 0.0 {
        tracing::debug!("[fees] goal {goal_id}: no fee (no profit)");
        return;
    }

    let treasury = treasury_address();
    if treasury.is_empty() && !dry_run {
        tracing::warn!("[fees] goal {goal_id}: VESPRA_TREASURY_ADDRESS not set, skipping fee tx");
        return;
    }

    let gain_eth = current_eth - entry_eth;
    let fee_wei = format!("{:.0}", fee * 1e18);

    //send fee through the same execution path as normal txs
    let tx_status = execution_gate::execute_traced(
        executor,
        config,
        chain_registry,
        http_client,
        goal_id,
        "WETH",
        &treasury,
        &fee_wei,
        chain,
        dry_run,
    )
    .await;

    let tx_hash = match &tx_status {
        TxStatus::Confirmed { tx_hash, .. } => {
            tracing::info!(
                "[fees] collected {fee:.6} ETH from goal {goal_id} (gain: {gain_eth:.6} ETH) tx={tx_hash}"
            );
            tx_hash.clone()
        }
        TxStatus::DryRun { calldata, .. } => {
            let cd_str = calldata.to_string();
            let hash = format!("dry-run:{}", &cd_str[..cd_str.len().min(16)]);
            tracing::info!(
                "[fees] collected {fee:.6} ETH from goal {goal_id} (gain: {gain_eth:.6} ETH) [DRY RUN]"
            );
            hash
        }
        other => {
            tracing::error!("[fees] fee tx failed for goal {goal_id}: {:?}", other);
            return;
        }
    };

    //record in redis
    let record = FeeRecord {
        goal_id,
        fee_eth: fee,
        fee_rate_bps: rate,
        tx_hash,
        timestamp: Utc::now().to_rfc3339(),
    };

    if let Ok(json) = serde_json::to_string(&record) {
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(redis).await
        {
            //per-goal record
            let _: Result<(), _> = conn.set(fee_key(&goal_id), &json).await;

            //running total (atomic increment)
            let _: Result<String, _> = redis::cmd("INCRBYFLOAT")
                    .arg(FEE_TOTAL_KEY)
                    .arg(fee)
                    .query_async(&mut conn).await;

            //daily total
            let today = Utc::now().format("%Y-%m-%d").to_string();
            let daily_key = format!("{FEE_TODAY_PREFIX}:{today}");
            let _: Result<String, _> = redis::cmd("INCRBYFLOAT")
                    .arg(&daily_key)
                    .arg(fee)
                    .query_async(&mut conn).await;
            let _: Result<(), _> = conn.expire(&daily_key, 172800).await; // 48h TTL

            //recent fees list
            let _: Result<(), _> = conn.lpush("fees:recent", &json).await;
            let _: Result<(), _> = conn.ltrim("fees:recent", 0, 99).await;
        }
    }

    //telegram notification
    if let Some(tg) = telegram {
        let tg = tg.clone();
        let msg = format!(
            "Fee collected \u{2014} goal {}, amount: {:.6} ETH",
            goal_id, fee
        );
        tokio::spawn(async move {
            let _ = tg.send(&msg).await;
        });
    }
}

///load fee record for a specific goal.
pub async fn get_fee_record(
    redis: &redis::Client,
    goal_id: Uuid,
) -> Option<FeeRecord> {
    let mut conn = redis::Client::get_multiplexed_async_connection(redis).await.ok()?;
    let raw: Option<String> = conn.get(fee_key(&goal_id)).await.ok()?;
    raw.and_then(|s| serde_json::from_str(&s).ok())
}

///load fee summary data.
pub async fn fee_summary(
    redis: &redis::Client,
) -> serde_json::Value {
    let rate = fee_rate_bps();
    let treasury = treasury_address();

    let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(redis).await else {
        return serde_json::json!({
            "total_fees_collected_eth": 0.0,
            "fees_today_eth": 0.0,
            "fee_rate_bps": rate,
            "treasury_address": treasury,
            "recent_fees": [],
            "error": "redis_unavailable",
        });
    };

    let total: f64 = conn
        .get::<_, Option<String>>(FEE_TOTAL_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let daily_key = format!("{FEE_TODAY_PREFIX}:{today}");
    let today_eth: f64 = conn
        .get::<_, Option<String>>(&daily_key)
        .await
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    let recent_raw: Vec<String> = conn
        .lrange("fees:recent", 0, 19)
        .await
        .unwrap_or_default();
    let recent: Vec<serde_json::Value> = recent_raw
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();

    serde_json::json!({
        "total_fees_collected_eth": total,
        "fees_today_eth": today_eth,
        "fee_rate_bps": rate,
        "treasury_address": treasury,
        "recent_fees": recent,
    })
}

//── tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_positive_gain() {
        //10 bps on 0.5 eth gain = 0.5 * 10/10000 = 0.0005 eth
        let fee = calculate_fee(1.0, 1.5, 10);
        assert!((fee - 0.0005).abs() < 1e-12, "fee={fee}");

        //50 bps on 0.2 eth gain = 0.2 * 50/10000 = 0.001 eth
        let fee = calculate_fee(0.1, 0.3, 50);
        assert!((fee - 0.001).abs() < 1e-12, "fee={fee}");
    }

    #[test]
    fn fee_zero_on_loss() {
        let fee = calculate_fee(1.0, 0.8, 10);
        assert_eq!(fee, 0.0, "no fee on loss");
    }

    #[test]
    fn fee_zero_on_breakeven() {
        let fee = calculate_fee(1.0, 1.0, 10);
        assert_eq!(fee, 0.0, "no fee on breakeven");
    }

    #[test]
    fn fee_rate_scales_correctly() {
        //100 bps = 1%
        let fee = calculate_fee(1.0, 2.0, 100);
        assert!((fee - 0.01).abs() < 1e-12, "1% of 1 ETH gain = 0.01");

        //1 bps = 0.01%
        let fee = calculate_fee(1.0, 2.0, 1);
        assert!((fee - 0.0001).abs() < 1e-12);
    }

    #[test]
    fn dry_run_does_not_affect_calculation() {
        //fee calculation is pure — dry_run only affects the tx broadcast,
        //not the calculation itself. verify the math is the same.
        let fee_normal = calculate_fee(0.5, 0.6, 10);
        let fee_dry = calculate_fee(0.5, 0.6, 10);
        assert_eq!(fee_normal, fee_dry);
        assert!(fee_normal > 0.0);
    }
}
