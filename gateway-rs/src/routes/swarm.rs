use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};

use super::AppState;

async fn swarm_kill(State(state): State<AppState>) -> Json<serde_json::Value> {
    // Set global kill flag
    state.kill_flag.store(true, Ordering::SeqCst);
    tracing::warn!("KILL SWITCH ACTIVATED");

    // Collect active wallet IDs and stop all loops
    let active = state.trade_up_orchestrator.active_wallets().await;
    let wallet_ids: Vec<String> = active.iter().map(|id| id.to_string()).collect();
    for wallet_id in &active {
        let _ = state.trade_up_orchestrator.stop_loop(*wallet_id).await;
    }

    Json(serde_json::json!({
        "status": "killed",
        "kill_flag": true,
        "loops_halted": wallet_ids,
    }))
}

async fn swarm_resume(State(state): State<AppState>) -> Json<serde_json::Value> {
    state.kill_flag.store(false, Ordering::SeqCst);
    tracing::info!("KILL SWITCH DEACTIVATED — swarm resumed");

    Json(serde_json::json!({
        "status": "resumed",
        "kill_flag": false,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/swarm/kill", post(swarm_kill))
        .route("/swarm/resume", post(swarm_resume))
}
