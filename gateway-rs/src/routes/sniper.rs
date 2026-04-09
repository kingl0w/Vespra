use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::AppState;
use crate::agents::sniper::SniperContext;
use crate::orchestrator::sniper::PoolEvent;
use crate::routes::goals::{save_goal, wallet_has_active_goal};
use crate::types::decisions::SniperDecision;
use crate::types::goals::{GoalSpec, GoalStatus, GoalStrategy};

//── alchemy webhook payload types ──────────────────────────────

#[derive(Debug, Deserialize)]
struct AlchemyWebhookBody {
    #[serde(default)]
    event: Option<AlchemyEvent>,
}

#[derive(Debug, Deserialize)]
struct AlchemyEvent {
    #[serde(default)]
    data: Option<AlchemyEventData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlchemyEventData {
    #[allow(dead_code)]
    #[serde(default)]
    block: Option<serde_json::Value>,
    #[serde(default)]
    logs: Option<Vec<serde_json::Value>>,
}

//── evaluation record stored in redis ──────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SniperEvaluation {
    timestamp: String,
    pool_address: String,
    token_pair: String,
    decision: String,
    confidence: f64,
    reason: String,
    auto_entered: bool,
}

const EVAL_LIST_KEY_PREFIX: &str = "sniper:evaluations";
const EVAL_LIST_MAX: isize = 100;

//── routes ─────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/webhooks/alchemy", post(alchemy_webhook))
        .route("/sniper/positions", get(sniper_positions))
        .route("/sniper/history", get(sniper_history))
        .route("/sniper/exit/:position_id", post(sniper_exit))
}

//── post /webhooks/alchemy ─────────────────────────────────────

async fn alchemy_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    //rate limit — webhook endpoint faces external traffic
    let client_ip = crate::routes::ratelimit::extract_client_ip(&headers);
    let (allowed, retry_after) = state.webhook_rate_limiter.check(&client_ip);
    if !allowed {
        let retry_ceil = retry_after.ceil() as u64;
        tracing::warn!("WEBHOOK_RATE_LIMIT ip={client_ip} retry_after={retry_ceil}s");
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", retry_ceil.to_string())],
            Json(serde_json::json!({
                "error": "rate limit exceeded",
                "retry_after": retry_ceil,
            })),
        )
            .into_response();
    }

    //step 1: validate hmac-sha256 signature
    let secret = &state.config.alchemy_webhook_secret;
    if !secret.is_empty() {
        let signature = headers
            .get("x-alchemy-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_hmac_sha256(secret, &body, signature) {
            tracing::warn!(
                "[sniper] invalid HMAC signature from ip={client_ip}"
            );
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "status": "error",
                    "error": "invalid_signature",
                })),
            )
                .into_response();
        }
    }

    //step 2: parse webhook body and extract pool events
    let parsed: AlchemyWebhookBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            //always return 200 to alchemy
            return Json(serde_json::json!({
                "status": "ok",
                "error": format!("parse_error: {e}"),
            }))
            .into_response();
        }
    };

    let logs = parsed
        .event
        .and_then(|e| e.data)
        .and_then(|d| d.logs)
        .unwrap_or_default();

    let mut results = Vec::new();
    let chain = state
        .config
        .chains
        .first()
        .cloned()
        .unwrap_or_else(|| "base".into());

    for log in &logs {
        let pool_address = log
            .get("address")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if pool_address.is_empty() {
            continue;
        }

        let topics = log
            .get("topics")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let token0 = topics
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let token1 = topics
            .get(2)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let fee_tier = topics
            .get(3)
            .and_then(|v| v.as_str())
            .unwrap_or("3000")
            .to_string();

        let block_number = log
            .get("blockNumber")
            .and_then(|v| v.as_str().or_else(|| v.as_u64().map(|_| "")))
            .unwrap_or("")
            .to_string();

        let token_pair = format!("{}/{}", short_addr(&token0), short_addr(&token1));

        //step 3: call sniper agent for evaluation
        let ctx = SniperContext {
            pool_address: pool_address.clone(),
            token0: token0.clone(),
            token1: token1.clone(),
            tvl_usd: 0.0, // TVL not available at creation time
            protocol: "uniswap-v3".into(),
            chain: chain.clone(),
            min_tvl_threshold: state.config.sniper_min_tvl,
        };

        let decision = match state.sniper_orchestrator.sniper.evaluate(&ctx).await {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("[sniper] agent evaluation failed for {pool_address}: {e}");
                SniperDecision::Pass {
                    reasoning: format!("agent error: {e}"),
                }
            }
        };

        let (decision_str, confidence, reason, suggested_eth) = match &decision {
            SniperDecision::Enter {
                confidence,
                max_entry_eth,
                reasoning,
            } => ("ENTER", *confidence, reasoning.clone(), *max_entry_eth),
            SniperDecision::Pass { reasoning } => ("SKIP", 0.0, reasoning.clone(), 0.0),
        };

        let mut auto_entered = false;

        //step 4: auto-entry if enter and auto_entry enabled
        if decision_str == "ENTER" && state.config.sniper_auto_entry_enabled {
            let position_eth = suggested_eth.min(state.config.sniper_max_entry_eth);

            //check no existing goalrunner for this pool_address
            let already_running = {
                let runners = state.goal_runners.lock().await;
                //check redis for goals referencing this pool
                let has_dup = if let Ok(mut conn) =
                    redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await
                {
                    let key = format!("sniper:pool:{pool_address}");
                    let exists: bool = conn.exists(&key).await.unwrap_or(false);
                    exists
                } else {
                    false
                };
                has_dup || runners.len() > 50 // safety cap
            };

            let _create_guard = state.goal_creation_lock.lock().await;

            let custody_label = state.config.default_custody.clone();
            if let Some(existing_id) =
                wallet_has_active_goal(&state.redis, &custody_label).await
            {
                tracing::info!(
                    "[sniper] auto-entry rejected — wallet {} already has active goal {} (pool {})",
                    custody_label,
                    existing_id,
                    pool_address
                );
                //drop the guard before continuing the loop so the next pool
                //event in this batch isn't blocked.
                drop(_create_guard);
                //skip to the evaluation persistence step below by leaving
                //auto_entered=false.
            } else if !already_running && position_eth > 0.0 {
                let target_gain = state.config.sniper_target_gain_pct;
                let stop_loss = state.config.sniper_stop_loss_pct;

                let goal = GoalSpec {
                    id: Uuid::new_v4(),
                    raw_goal: format!(
                        "Sniper auto-entry: {token_pair} pool {pool_address}"
                    ),
                    wallet_label: state.config.default_custody.clone(),
                    wallet_id: None, // resolved lazily by runner; sniper auto-entries pre-date this field
                    chain: chain.clone(),
                    capital_eth: position_eth,
                    target_gain_pct: target_gain,
                    stop_loss_pct: stop_loss,
                    strategy: GoalStrategy::Snipe,
                    status: GoalStatus::Running,
                    cycles: 0,
                    current_step: "SCOUTING".into(),
                    entry_eth: position_eth,
                    current_eth: position_eth,
                    pnl_eth: 0.0,
                    pnl_pct: 0.0,
                    token_address: None,
                    token_amount_held: None,
                    resolved_wallet_uuid: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    error: None,
                };

                if let Err(e) = save_goal(&state.redis, &goal).await {
                    tracing::error!("[sniper] failed to save goal: {e}");
                } else {
                    //mark pool as tracked
                    if let Ok(mut conn) =
                        redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await
                    {
                        let key = format!("sniper:pool:{pool_address}");
                        let _: Result<(), _> = conn.set_ex(&key, goal.id.to_string(), 86400).await;
                    }

                    //spawn goalrunner
                    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
                    let goal_id = goal.id;
                    let deps = state.goal_runner_deps.clone();
                    let runners_for_cleanup = state.goal_runners.clone();
                    let txs_for_cleanup = state.goal_cancel_txs.clone();
                    let handle = tokio::spawn(async move {
                        crate::goal_runner::run_goal(goal_id, cancel_rx, deps).await;
                        //ves-mem: clean up shared maps when the runner exits.
                        runners_for_cleanup.lock().await.remove(&goal_id);
                        txs_for_cleanup.lock().await.remove(&goal_id);
                    });

                    {
                        let mut runners = state.goal_runners.lock().await;
                        runners.insert(goal_id, handle);
                    }
                    {
                        let mut txs = state.goal_cancel_txs.lock().await;
                        txs.insert(goal_id, cancel_tx);
                    }

                    auto_entered = true;
                    tracing::info!(
                        "[sniper] AUTO ENTRY: {pool_address} confidence={confidence} \
                         position={position_eth} ETH goal={goal_id}"
                    );
                }
            }
        }

        //step 5: store evaluation in redis
        let eval = SniperEvaluation {
            timestamp: Utc::now().to_rfc3339(),
            pool_address: pool_address.clone(),
            token_pair: token_pair.clone(),
            decision: decision_str.to_string(),
            confidence,
            reason: reason.clone(),
            auto_entered,
        };

        if let Ok(json) = serde_json::to_string(&eval) {
            if let Ok(mut conn) =
                redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await
            {
                let today = Utc::now().format("%Y-%m-%d").to_string();
                let key = format!("{EVAL_LIST_KEY_PREFIX}:{today}");
                let _: Result<(), _> = conn.lpush(&key, &json).await;
                let _: Result<(), _> = conn.ltrim(&key, 0, EVAL_LIST_MAX - 1).await;
                let _: Result<(), _> = conn.expire(&key, 172800).await; // 48h TTL

                //also push to the legacy key for backward compat with /sniper/history
                let _: Result<(), _> = conn.lpush("vespra:sniper_history", &json).await;
                let _: Result<(), _> = conn.ltrim("vespra:sniper_history", 0, 99).await;
            }
        }

        //also run through the orchestrator for existing position tracking
        let event = PoolEvent {
            pool_address,
            token0,
            token1,
            tvl_usd: 0.0,
            protocol: "uniswap-v3".into(),
            chain: chain.clone(),
        };
        let orch_result = state
            .sniper_orchestrator
            .evaluate_pool(event, Uuid::nil())
            .await;
        results.push(serde_json::json!({
            "decision": decision_str,
            "confidence": confidence,
            "reason": reason,
            "auto_entered": auto_entered,
            "fee_tier": fee_tier,
            "block_number": block_number,
            "orchestrator": serde_json::to_value(&orch_result).unwrap_or_default(),
        }));
    }

    //step 6: always return 200 to alchemy
    Json(serde_json::json!({
        "status": "ok",
        "events": logs.len(),
        "results": results,
    }))
    .into_response()
}

//── get /sniper/positions ──────────────────────────────────────

async fn sniper_positions(State(state): State<AppState>) -> Json<serde_json::Value> {
    let entries = state.sniper_orchestrator.active_positions().await;
    Json(serde_json::json!({
        "count": entries.len(),
        "positions": entries,
    }))
}

//── get /sniper/history ────────────────────────────────────────

async fn sniper_history(State(state): State<AppState>) -> Json<serde_json::Value> {
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            let raw: Vec<String> = conn
                .lrange("vespra:sniper_history", 0, 49)
                .await
                .unwrap_or_default();
            let entries: Vec<serde_json::Value> = raw
                .iter()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();
            Json(serde_json::json!({
                "count": entries.len(),
                "entries": entries,
            }))
        }
        Err(_) => Json(serde_json::json!({
            "count": 0,
            "entries": [],
            "error": "redis_unavailable",
        })),
    }
}

//── post /sniper/exit/:position_id ─────────────────────────────

#[derive(Debug, Deserialize)]
struct ExitRequest {
    wallet_id: Option<Uuid>,
}

async fn sniper_exit(
    State(state): State<AppState>,
    Path(position_id): Path<String>,
    Json(body): Json<ExitRequest>,
) -> Json<serde_json::Value> {
    let wallet_id = body.wallet_id.unwrap_or(Uuid::nil());
    let result = state
        .sniper_orchestrator
        .exit_position(&position_id, wallet_id)
        .await;
    Json(serde_json::to_value(&result).unwrap_or_default())
}

//── hmac-sha256 verification ───────────────────────────────────

pub fn verify_hmac_sha256(secret: &str, body: &[u8], signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let expected = hex::encode(mac.finalize().into_bytes());
    expected == signature
}

fn short_addr(addr: &str) -> &str {
    if addr.len() > 10 {
        &addr[..10]
    } else {
        addr
    }
}

//── tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_rejects_invalid_signature() {
        let secret = "test_secret_key_12345";
        let body = b"hello world";

        //valid signature
        let valid_sig = {
            use hmac::{Hmac, Mac};
            use sha2::Sha256;
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(body);
            hex::encode(mac.finalize().into_bytes())
        };
        assert!(verify_hmac_sha256(secret, body, &valid_sig));

        //invalid signature
        assert!(!verify_hmac_sha256(secret, body, "deadbeef"));
        assert!(!verify_hmac_sha256(secret, body, ""));

        //wrong body
        assert!(!verify_hmac_sha256(secret, b"wrong body", &valid_sig));

        //wrong secret
        assert!(!verify_hmac_sha256("wrong_secret", body, &valid_sig));
    }

    #[test]
    fn evaluation_serializes_correctly() {
        let eval = SniperEvaluation {
            timestamp: "2026-04-03T22:00:00Z".into(),
            pool_address: "0xabc123".into(),
            token_pair: "0xabc123/0xdef456".into(),
            decision: "ENTER".into(),
            confidence: 0.85,
            reason: "high initial liquidity".into(),
            auto_entered: true,
        };
        let json = serde_json::to_string(&eval).unwrap();
        let parsed: SniperEvaluation = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.decision, "ENTER");
        assert_eq!(parsed.pool_address, "0xabc123");
        assert!(parsed.auto_entered);
        assert!((parsed.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn auto_entry_respects_max_position_cap() {
        let suggested_eth: f64 = 0.10;
        let max_position_eth: f64 = 0.05;
        let capped = suggested_eth.min(max_position_eth);
        assert!(
            (capped - 0.05).abs() < f64::EPSILON,
            "position should be capped to {max_position_eth}"
        );

        //under cap — no change
        let suggested_small: f64 = 0.03;
        let capped_small = suggested_small.min(max_position_eth);
        assert!(
            (capped_small - 0.03).abs() < f64::EPSILON,
            "position below cap should stay at {suggested_small}"
        );
    }
}
