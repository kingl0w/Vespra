use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use redis::AsyncCommands;

use super::AppState;

async fn fee_summary(State(state): State<AppState>) -> Json<serde_json::Value> {
    // Read fee records written by Keymaster from Redis
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            let total: f64 = conn
                .get::<_, Option<String>>("vespra:fees:total_eth")
                .await
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let pending: f64 = conn
                .get::<_, Option<String>>("vespra:fees:pending_eth")
                .await
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);
            let strategies_raw: Option<String> = conn
                .get("vespra:fees:strategies")
                .await
                .ok()
                .flatten();
            let strategies: serde_json::Value = strategies_raw
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::json!({}));

            Json(serde_json::json!({
                "total_fees_eth": total,
                "pending_fees_eth": pending,
                "strategies": strategies,
            }))
        }
        Err(_) => Json(serde_json::json!({
            "total_fees_eth": 0.0,
            "pending_fees_eth": 0.0,
            "strategies": {},
            "error": "redis_unavailable",
        })),
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/fees/summary", get(fee_summary))
}
