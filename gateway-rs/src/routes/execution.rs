use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::execution_gate;

use super::AppState;

async fn validate(State(state): State<AppState>) -> Json<serde_json::Value> {
    let checks = execution_gate::run_validation_checks(
        &state.config,
        &state.chain_registry,
        &state.goal_runner_deps.quote_fetcher,
    )
    .await;

    let all_ok = checks.iter().all(|c| c.ok);

    Json(serde_json::json!({
        "checks": checks,
        "all_ok": all_ok,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/execution/validate", get(validate))
}
