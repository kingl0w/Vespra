use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use redis::AsyncCommands;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/goals/boot/summary", get(boot_summary))
}

async fn boot_summary(State(state): State<AppState>) -> impl IntoResponse {
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            let raw: Option<String> = conn
                .get("boot:last_resume_report")
                .await
                .unwrap_or(None);
            match raw.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
                Some(report) => axum::Json(report),
                None => axum::Json(serde_json::json!({
                    "booted_at": null,
                    "goals_resumed": 0,
                    "from_monitoring": 0,
                    "from_scouting": 0,
                    "paused_count": 0,
                })),
            }
        }
        Err(_) => axum::Json(serde_json::json!({
            "error": "redis_unavailable",
        })),
    }
}
