use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::agents::chat::ChatHandler;
use crate::routes::goals::list_goals_by_status;
use crate::types::goals::GoalStatus;

use super::AppState;

pub(crate) async fn propagate_kill_switch_to_keymaster(
    client: &reqwest::Client,
    keymaster_url: &str,
    auth_token: &str,
    activate: bool,
) -> Result<(), String> {
    let action = if activate { "activate" } else { "deactivate" };
    let url = format!(
        "{}/kill-switch/{action}",
        keymaster_url.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {auth_token}"))
        .send()
        .await
        .map_err(|e| format!("keymaster unreachable: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("keymaster returned {status}: {body}"));
    }
    Ok(())
}

async fn swarm_kill(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    //keymaster is the source of truth — if it can't be reached, refuse to
    //claim the swarm is killed. gateway compromise alone must not be able
    //to enable or disable signing.
    if let Err(e) = propagate_kill_switch_to_keymaster(
        &state.http_client,
        &state.config.keymaster_url,
        &state.config.keymaster_token,
        true,
    )
    .await
    {
        tracing::error!("[kill_switch] keymaster propagation failed: {e}");
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "keymaster unreachable — kill switch not activated",
                "detail": e,
            })),
        ));
    }

    //keymaster accepted — now mirror locally so gateway loops stop too.
    state.kill_flag.store(true, Ordering::SeqCst);
    tracing::warn!("KILL SWITCH ACTIVATED");
    crate::notifications::notify(
        &state,
        "\u{1F6D1} Kill switch ACTIVATED \u{2014} signing disabled at Keymaster".to_string(),
    );

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

    Ok(Json(serde_json::json!({
        "status": "killed",
        "kill_flag": true,
        "keymaster_kill_switch": "activated",
        "loops_halted": halted,
    })))
}

async fn swarm_resume(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    if let Err(e) = propagate_kill_switch_to_keymaster(
        &state.http_client,
        &state.config.keymaster_url,
        &state.config.keymaster_token,
        false,
    )
    .await
    {
        tracing::error!("[kill_switch] keymaster propagation failed: {e}");
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "keymaster unreachable — kill switch not deactivated",
                "detail": e,
            })),
        ));
    }

    state.kill_flag.store(false, Ordering::SeqCst);
    tracing::info!("KILL SWITCH DEACTIVATED — swarm resumed");
    crate::notifications::notify(
        &state,
        "\u{2705} Kill switch DEACTIVATED \u{2014} signing re-enabled at Keymaster".to_string(),
    );

    Ok(Json(serde_json::json!({
        "status": "resumed",
        "kill_flag": false,
        "keymaster_kill_switch": "deactivated",
    })))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Arc;

    #[tokio::test]
    async fn propagate_returns_err_when_keymaster_unreachable() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        //127.0.0.1:1 has no listener — connection refused immediately.
        let result =
            propagate_kill_switch_to_keymaster(&client, "http://127.0.0.1:1", "token", true).await;
        let err = result.expect_err("expected err when keymaster unreachable");
        assert!(
            err.contains("keymaster unreachable"),
            "error should mention unreachable keymaster, got: {err}"
        );
    }

    #[tokio::test]
    async fn propagate_calls_activate_path_on_keymaster() {
        use axum::routing::post;
        use axum::{Json, Router};

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();

        let app = Router::new().route(
            "/kill-switch/activate",
            post(move || {
                let hits = hits_clone.clone();
                async move {
                    hits.fetch_add(1, AtomicOrdering::SeqCst);
                    Json(serde_json::json!({ "active": true }))
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");
        let result = propagate_kill_switch_to_keymaster(&client, &base, "token", true).await;
        assert!(result.is_ok(), "expected ok, got: {result:?}");
        assert_eq!(hits.load(AtomicOrdering::SeqCst), 1);

        server.abort();
    }

    #[tokio::test]
    async fn propagate_calls_deactivate_path_on_keymaster() {
        use axum::routing::post;
        use axum::{Json, Router};

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();

        let app = Router::new().route(
            "/kill-switch/deactivate",
            post(move || {
                let hits = hits_clone.clone();
                async move {
                    hits.fetch_add(1, AtomicOrdering::SeqCst);
                    Json(serde_json::json!({ "active": false }))
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");
        let result = propagate_kill_switch_to_keymaster(&client, &base, "token", false).await;
        assert!(result.is_ok(), "expected ok, got: {result:?}");
        assert_eq!(hits.load(AtomicOrdering::SeqCst), 1);

        server.abort();
    }
}
