use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/yield/scheduler/status", get(scheduler_status))
}

async fn scheduler_status(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.yield_scheduler_status.read().await;
    axum::Json(serde_json::json!({
        "running": status.running,
        "last_run": status.last_run,
        "positions_monitored": status.positions_monitored,
        "rotations_today": status.rotations_today,
        "next_run": status.next_run,
    }))
}
