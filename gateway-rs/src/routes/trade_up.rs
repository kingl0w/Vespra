use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use redis::AsyncCommands;
use serde::Deserialize;
use uuid::Uuid;

use super::AppState;

//─── existing cycle-based routes ────────────────────────────────

#[derive(Debug, Deserialize)]
struct StartRequest {
    wallet_id: Uuid,
    capital_eth: f64,
    chains: Option<Vec<String>>,
}

async fn start_trade_up(
    State(state): State<AppState>,
    Json(body): Json<StartRequest>,
) -> Json<serde_json::Value> {
    let chains = body
        .chains
        .unwrap_or_else(|| state.config.chains.clone());

    match state
        .trade_up_orchestrator
        .start_loop(body.wallet_id, body.capital_eth, chains)
        .await
    {
        Ok(()) => Json(serde_json::json!({
            "status": "started",
            "wallet_id": body.wallet_id,
            "capital_eth": body.capital_eth,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

async fn stop_trade_up(
    State(state): State<AppState>,
    Path(wallet_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    match state.trade_up_orchestrator.stop_loop(wallet_id).await {
        Ok(()) => Json(serde_json::json!({
            "status": "stop_requested",
            "wallet_id": wallet_id,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

async fn trade_up_status(
    State(state): State<AppState>,
    Path(wallet_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let active_wallets = state.trade_up_orchestrator.active_wallets().await;
    let is_active = active_wallets.contains(&wallet_id);

    //read last cycle result from redis
    let (cycle, capital, last_status) = match redis::Client::get_multiplexed_async_connection(
        state.redis.as_ref(),
    )
    .await
    {
        Ok(mut conn) => {
            let key = format!("vespra:trade_up_state:{wallet_id}");
            let raw: Option<String> = conn.get(&key).await.ok().flatten();
            match raw.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
                Some(v) => (
                    v.get("cycle").and_then(|c| c.as_u64()).unwrap_or(0) as u32,
                    v.get("capital_eth")
                        .and_then(|c| c.as_f64())
                        .unwrap_or(0.0),
                    v.get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                ),
                None => (0, 0.0, "no_data".into()),
            }
        }
        Err(_) => (0, 0.0, "redis_unavailable".into()),
    };

    Json(serde_json::json!({
        "active": is_active,
        "cycle": cycle,
        "capital_eth": capital,
        "last_status": last_status,
    }))
}

async fn trade_up_history(State(state): State<AppState>) -> Json<serde_json::Value> {
    read_history_from_redis(&state, "vespra:trade_up_history").await
}

async fn trade_up_wallet_history(
    State(state): State<AppState>,
    Path(wallet_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let key = format!("vespra:trade_up_history:{wallet_id}");
    read_history_from_redis(&state, &key).await
}

async fn read_history_from_redis(state: &AppState, key: &str) -> Json<serde_json::Value> {
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            let raw: Vec<String> = conn
                .lrange(key, 0, 99)
                .await
                .unwrap_or_default();
            let cycles: Vec<serde_json::Value> = raw
                .iter()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();
            Json(serde_json::json!({
                "count": cycles.len(),
                "cycles": cycles,
            }))
        }
        Err(_) => Json(serde_json::json!({
            "count": 0,
            "cycles": [],
            "error": "redis_unavailable",
        })),
    }
}

//─── ves-37: position-based routes ──────────────────────────────

#[derive(Debug, Deserialize)]
struct PositionStartRequest {
    wallet: String,
    chain: String,
}

async fn start_position_loop(
    State(state): State<AppState>,
    Json(body): Json<PositionStartRequest>,
) -> Json<serde_json::Value> {
    match state
        .trade_up_orchestrator
        .start_position_loop(body.wallet.clone(), body.chain.clone())
        .await
    {
        Ok(()) => Json(serde_json::json!({
            "status": "started",
            "wallet": body.wallet,
            "chain": body.chain,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

async fn stop_position_loop(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    match state.trade_up_orchestrator.stop_all_loops().await {
        Ok(()) => Json(serde_json::json!({
            "status": "stopped",
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

async fn position_status(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    let phase = state
        .trade_up_orchestrator
        .get_loop_phase()
        .await
        .map(|p| p.to_string())
        .unwrap_or_else(|| "idle".into());

    let active_position = state
        .trade_up_orchestrator
        .get_active_position()
        .await
        .ok()
        .flatten();

    let pnl = active_position.as_ref().map(|pos| {
        serde_json::json!({
            "entry_eth": pos.entry_eth,
            "entry_price_usd": pos.entry_price_usd,
            "token_symbol": pos.token_symbol,
            "opened_at": pos.opened_at,
        })
    });

    Json(serde_json::json!({
        "loop_phase": phase,
        "active_position": active_position,
        "current_pnl": pnl,
    }))
}

async fn position_history(
    State(state): State<AppState>,
) -> Json<serde_json::Value> {
    match state.trade_up_orchestrator.get_all_positions().await {
        Ok(positions) => Json(serde_json::json!({
            "count": positions.len(),
            "positions": positions,
        })),
        Err(e) => Json(serde_json::json!({
            "count": 0,
            "positions": [],
            "error": e.to_string(),
        })),
    }
}

//─── router ─────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        //existing cycle-based endpoints
        .route("/trade-up/start", post(start_trade_up))
        .route("/trade-up/stop/:wallet_id", post(stop_trade_up))
        .route("/trade-up/status/:wallet_id", get(trade_up_status))
        .route("/trade-up/history", get(trade_up_history))
        .route("/trade-up/history/:wallet_id", get(trade_up_wallet_history))
        //ves-37: position-based endpoints
        .route("/trade-up/position/start", post(start_position_loop))
        .route("/trade-up/position/stop", post(stop_position_loop))
        .route("/trade-up/position/status", get(position_status))
        .route("/trade-up/position/history", get(position_history))
}
