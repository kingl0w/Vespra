use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::AppState;

async fn swarm_kill(State(state): State<AppState>) -> Json<serde_json::Value> {
    // Set global kill flag
    state.kill_flag.store(true, Ordering::SeqCst);
    tracing::warn!("KILL SWITCH ACTIVATED");

    // Collect active wallet IDs and stop all loops
    let trade_up_active = state.trade_up_orchestrator.active_wallets().await;
    let yield_active = state.yield_orchestrator.active_wallets().await;
    let mut halted = Vec::new();
    for wallet_id in &trade_up_active {
        let _ = state.trade_up_orchestrator.stop_loop(*wallet_id).await;
        halted.push(format!("trade_up:{wallet_id}"));
    }
    for wallet_id in &yield_active {
        let _ = state.yield_orchestrator.stop_loop(*wallet_id).await;
        halted.push(format!("yield:{wallet_id}"));
    }

    Json(serde_json::json!({
        "status": "killed",
        "kill_flag": true,
        "loops_halted": halted,
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

#[derive(Debug, Deserialize)]
struct CommandRequest {
    command: String,
    wallet_id: Option<String>,
}

async fn swarm_command(
    State(state): State<AppState>,
    Json(body): Json<CommandRequest>,
) -> Json<serde_json::Value> {
    let report = state
        .command_orchestrator
        .execute(body.command, body.wallet_id)
        .await;
    Json(serde_json::to_value(&report).unwrap_or_default())
}

async fn swarm_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let trade_up_wallets = state.trade_up_orchestrator.active_wallets().await;
    let yield_wallets = state.yield_orchestrator.active_wallets().await;
    let sniper_positions = state.sniper_orchestrator.active_positions().await;

    Json(serde_json::json!({
        "kill_flag": state.kill_flag.load(Ordering::SeqCst),
        "chains": state.config.chains,
        "trade_up_loops": trade_up_wallets.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "yield_loops": yield_wallets.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
        "sniper_positions": sniper_positions.len(),
        "total_active_loops": trade_up_wallets.len() + yield_wallets.len(),
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/swarm/kill", post(swarm_kill))
        .route("/swarm/resume", post(swarm_resume))
        .route("/swarm/command", post(swarm_command))
        .route("/swarm/status", get(swarm_status))
}
