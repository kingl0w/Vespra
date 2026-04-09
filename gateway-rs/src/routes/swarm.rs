use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::agents::chat::ChatHandler;
use crate::routes::goals::list_goals_by_status;
use crate::types::goals::GoalStatus;

use super::AppState;

async fn swarm_kill(State(state): State<AppState>) -> Json<serde_json::Value> {
    //set global kill flag
    state.kill_flag.store(true, Ordering::SeqCst);
    tracing::warn!("KILL SWITCH ACTIVATED");

    //collect active wallet ids and stop all loops
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
    command: Option<String>,
    message: Option<String>,
    #[allow(dead_code)]
    wallet_id: Option<String>,
}

async fn swarm_command(
    State(state): State<AppState>,
    Json(body): Json<CommandRequest>,
) -> Json<serde_json::Value> {
    let user_msg = body.command
        .or(body.message)
        .unwrap_or_default();

    if user_msg.trim().is_empty() {
        return Json(serde_json::json!({
            "message": "Please send a message."
        }));
    }

    //build live context from system state
    let live_context = build_live_context(&state).await;

    //use chathandler for natural language responses
    let chat = ChatHandler::new(state.llm.clone());
    match chat.respond(&user_msg, &live_context).await {
        Ok(response) => Json(serde_json::json!({
            "message": response,
        })),
        Err(e) => {
            tracing::warn!("[chat] LLM call failed: {e}");
            Json(serde_json::json!({
                "message": "I'm having trouble connecting to my language model right now. Try again in a moment.",
            }))
        }
    }
}

async fn build_live_context(state: &AppState) -> String {
    let mut lines = Vec::new();

    //active goals
    if let Ok(running) = list_goals_by_status(&state.redis, GoalStatus::Running).await {
        if running.is_empty() {
            lines.push("No goals currently running.".into());
        } else {
            lines.push(format!("{} goal(s) running:", running.len()));
            for g in &running {
                lines.push(format!(
                    "  - Goal {}: strategy={:?}, step={}, capital={:.4} ETH, P&L={:+.4} ETH ({:+.1}%)",
                    &g.id.to_string()[..8], g.strategy, g.current_step,
                    g.capital_eth, g.pnl_eth, g.pnl_pct
                ));
            }
        }
    }

    //paused goals
    if let Ok(paused) = list_goals_by_status(&state.redis, GoalStatus::Paused).await {
        if !paused.is_empty() {
            lines.push(format!("{} goal(s) paused.", paused.len()));
        }
    }

    //sentinel status
    {
        let sentinel = state.sentinel_monitor.status.read().await;
        let last_run = sentinel.last_run
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into());
        lines.push(format!(
            "Sentinel monitor: running={}, last_run={}, goals_monitored={}",
            sentinel.running, last_run, sentinel.goals_monitored
        ));
    }

    //yield scheduler status
    {
        let ys = state.yield_scheduler_status.read().await;
        let last_run = ys.last_run
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into());
        lines.push(format!(
            "Yield scheduler: running={}, last_run={}, positions_monitored={}",
            ys.running, last_run, ys.positions_monitored
        ));
    }

    //kill flag
    let killed = state.kill_flag.load(Ordering::SeqCst);
    if killed {
        lines.push("WARNING: Kill switch is ACTIVE. All loops halted.".into());
    }

    lines.join("\n")
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
