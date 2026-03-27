use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};

use super::AppState;

async fn swarm_kill(State(state): State<AppState>) -> Json<serde_json::Value> {
    // Set global kill flag
    state.kill_flag.store(true, Ordering::SeqCst);

    // Stop all active trade-up loops
    let active = state.trade_up_orchestrator.active_wallets().await;
    let count = active.len();
    for wallet_id in active {
        let _ = state.trade_up_orchestrator.stop_loop(wallet_id).await;
    }

    Json(serde_json::json!({
        "status": "killed",
        "loops_stopped": count,
    }))
}

async fn swarm_resume(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.kill_flag.store(false, Ordering::SeqCst);

    Json(serde_json::json!({
        "status": "resumed",
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/swarm/kill", post(swarm_kill))
        .route("/swarm/resume", post(swarm_resume))
}
