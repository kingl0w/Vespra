use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/sentinel/status", get(sentinel_status))
}

async fn sentinel_status(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.sentinel_monitor.status.read().await;
    axum::Json(serde_json::json!({
        "running": status.running,
        "last_run": status.last_run,
        "goals_monitored": status.goals_monitored,
        "signals_sent_today": status.signals_sent_today,
    }))
}
