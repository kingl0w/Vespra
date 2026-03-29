use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use redis::AsyncCommands;
use serde::Deserialize;
use uuid::Uuid;

use super::AppState;
use crate::orchestrator::sniper::PoolEvent;

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
    #[serde(default)]
    block: Option<serde_json::Value>,
    #[serde(default)]
    logs: Option<Vec<serde_json::Value>>,
}

async fn alchemy_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    // Rate limit — webhook endpoint faces external traffic
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

    // Validate HMAC-SHA256 signature
    let secret = &state.config.alchemy_webhook_secret;
    if !secret.is_empty() {
        let signature = headers
            .get("x-alchemy-signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !verify_hmac_sha256(secret, &body, signature) {
            return Json(serde_json::json!({
                "status": "error",
                "error": "invalid_signature",
            })).into_response();
        }
    }

    // Parse webhook body
    let parsed: AlchemyWebhookBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "error",
                "error": format!("parse_error: {e}"),
            })).into_response();
        }
    };

    // Extract pool creation events from logs
    let logs = parsed
        .event
        .and_then(|e| e.data)
        .and_then(|d| d.logs)
        .unwrap_or_default();

    let mut results = Vec::new();
    for log in &logs {
        // Extract pool event fields — adapt based on actual Alchemy log format
        let pool_address = log.get("address")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if pool_address.is_empty() {
            continue;
        }

        let topics = log.get("topics")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let token0 = topics.get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let token1 = topics.get(2)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let event = PoolEvent {
            pool_address,
            token0,
            token1,
            tvl_usd: 0.0, // TVL not available at creation time
            protocol: "uniswap-v3".into(),
            chain: state.config.chains.first().cloned().unwrap_or_else(|| "base".into()),
        };

        // Use a default wallet_id for webhook-driven entries
        let wallet_id = Uuid::nil();
        let result = state.sniper_orchestrator.evaluate_pool(event, wallet_id).await;
        results.push(serde_json::to_value(&result).unwrap_or_default());
    }

    Json(serde_json::json!({
        "status": "processed",
        "events": logs.len(),
        "results": results,
    })).into_response()
}

async fn sniper_positions(State(state): State<AppState>) -> Json<serde_json::Value> {
    let entries = state.sniper_orchestrator.active_positions().await;
    Json(serde_json::json!({
        "count": entries.len(),
        "positions": entries,
    }))
}

async fn sniper_history(State(state): State<AppState>) -> Json<serde_json::Value> {
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            let raw: Vec<String> = conn.lrange("vespra:sniper_history", 0, 99).await.unwrap_or_default();
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
    let result = state.sniper_orchestrator.exit_position(&position_id, wallet_id).await;
    Json(serde_json::to_value(&result).unwrap_or_default())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/webhooks/alchemy", post(alchemy_webhook))
        .route("/sniper/positions", get(sniper_positions))
        .route("/sniper/history", get(sniper_history))
        .route("/sniper/exit/:position_id", post(sniper_exit))
}

fn verify_hmac_sha256(secret: &str, body: &[u8], signature: &str) -> bool {
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
