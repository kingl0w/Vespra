use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use redis::AsyncCommands;
use serde::Deserialize;
use std::collections::BTreeSet;
use uuid::Uuid;

use crate::data::aave;
use super::AppState;

#[derive(Debug, Deserialize)]
struct ProtocolsQuery {
    chain: Option<String>,
}

async fn yield_protocols(
    State(state): State<AppState>,
    Query(params): Query<ProtocolsQuery>,
) -> Json<serde_json::Value> {
    //determine which chains to query
    let chains: Vec<String> = match &params.chain {
        Some(c) => vec![c.clone()],
        None => state.config.chains.clone(),
    };

    //resolve chain names → defillama slugs via chainregistry
    let mut slug_to_chain: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for name in &chains {
        if let Some(cfg) = state.chain_registry.get(name) {
            slug_to_chain.insert(cfg.defillama_slug.to_lowercase(), name.clone());
        }
    }

    if slug_to_chain.is_empty() {
        return Json(serde_json::json!({
            "chain": params.chain.as_deref().unwrap_or("all"),
            "protocols": [],
            "count": 0,
            "error": "no matching chains in registry",
        }));
    }

    //fetch pools from defi llama
    let client = state.http_client.clone();

    let resp = match client.get("https://yields.llama.fi/pools").send().await {
        Ok(r) => r,
        Err(e) => {
            return Json(serde_json::json!({
                "chain": params.chain.as_deref().unwrap_or("all"),
                "protocols": [],
                "count": 0,
                "error": format!("pool fetch failed: {e}"),
            }));
        }
    };

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return Json(serde_json::json!({
                "chain": params.chain.as_deref().unwrap_or("all"),
                "protocols": [],
                "count": 0,
                "error": format!("parse failed: {e}"),
            }));
        }
    };

    let pools = body.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();

    let mut protocols = BTreeSet::new();
    for pool in &pools {
        let pool_chain = pool.get("chain").and_then(|v| v.as_str()).unwrap_or("");
        if slug_to_chain.contains_key(&pool_chain.to_lowercase()) {
            if let Some(project) = pool.get("project").and_then(|v| v.as_str()) {
                if !project.is_empty() {
                    protocols.insert(project.to_string());
                }
            }
        }
    }

    let protocol_list: Vec<String> = protocols.into_iter().collect();
    let count = protocol_list.len();

    Json(serde_json::json!({
        "chain": params.chain.as_deref().unwrap_or("all"),
        "protocols": protocol_list,
        "count": count,
    }))
}

//─── yield loop control ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct YieldStartRequest {
    wallet_id: Uuid,
    capital_eth: f64,
    chain: String,
}

async fn yield_start(
    State(state): State<AppState>,
    Json(body): Json<YieldStartRequest>,
) -> Json<serde_json::Value> {
    match state
        .yield_orchestrator
        .start_loop(body.wallet_id, body.capital_eth, body.chain.clone())
        .await
    {
        Ok(()) => Json(serde_json::json!({
            "status": "started",
            "wallet_id": body.wallet_id,
            "capital_eth": body.capital_eth,
            "chain": body.chain,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

async fn yield_stop(
    State(state): State<AppState>,
    Path(wallet_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    match state.yield_orchestrator.stop_loop(wallet_id).await {
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

async fn yield_status(
    State(state): State<AppState>,
    Path(wallet_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let active_wallets = state.yield_orchestrator.active_wallets().await;
    let is_active = active_wallets.contains(&wallet_id);

    let (cycle, capital, last_status) = match redis::Client::get_multiplexed_async_connection(
        state.redis.as_ref(),
    )
    .await
    {
        Ok(mut conn) => {
            let key = format!("vespra:yield_state:{wallet_id}");
            let raw: Option<String> = conn.get(&key).await.ok().flatten();
            match raw.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
                Some(v) => (
                    v.get("cycle").and_then(|c| c.as_u64()).unwrap_or(0) as u32,
                    v.get("capital_eth").and_then(|c| c.as_f64()).unwrap_or(0.0),
                    v.get("status").and_then(|s| s.as_str()).unwrap_or("unknown").to_string(),
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

async fn yield_history(
    State(state): State<AppState>,
    Path(wallet_id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let key = format!("vespra:yield_rotations:{wallet_id}");
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            let raw: Vec<String> = conn.lrange(&key, 0, 99).await.unwrap_or_default();
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

//─── raw yield pool data (debug / dashboard) ───────────────────

#[derive(Debug, Deserialize)]
struct YieldPoolsQuery {
    chain: Option<String>,
    min_apy: Option<f64>,
    min_tvl: Option<f64>,
    limit: Option<usize>,
}

async fn yield_pools(
    State(state): State<AppState>,
    Query(params): Query<YieldPoolsQuery>,
) -> Json<serde_json::Value> {
    let min_tvl = params.min_tvl.unwrap_or(state.config.yield_min_tvl_usd);
    let min_apy = params.min_apy.unwrap_or(state.config.yield_min_apy);
    let limit = params.limit.unwrap_or(state.config.yield_top_n);

    match state
        .yield_registry
        .fetch_pools(params.chain.as_deref(), min_tvl, min_apy)
        .await
    {
        Ok(mut pools) => {
            pools.truncate(limit);
            let count = pools.len();
            Json(serde_json::json!({
                "pools": pools,
                "count": count,
                "filters": {
                    "chain": params.chain.as_deref().unwrap_or("all"),
                    "min_apy": min_apy,
                    "min_tvl_usd": min_tvl,
                    "limit": limit,
                },
            }))
        }
        Err(e) => Json(serde_json::json!({
            "pools": [],
            "count": 0,
            "error": e.to_string(),
        })),
    }
}

//─── aave v3 position data ───────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PositionsQuery {
    wallet: String,
    chain: String,
}

async fn yield_positions(
    State(state): State<AppState>,
    Query(params): Query<PositionsQuery>,
) -> Json<serde_json::Value> {
    //resolve wallet label/id to address via keymaster
    let address = match aave::resolve_wallet_address(
        &state.http_client,
        &state.config.keymaster_url,
        &state.config.keymaster_token,
        &params.wallet,
        &params.chain,
    )
    .await
    {
        Ok(addr) => addr,
        Err(e) => {
            return Json(serde_json::json!({
                "positions": [],
                "error": format!("wallet resolution failed: {e}"),
            }));
        }
    };

    match state
        .aave_fetcher
        .fetch_positions_enriched(
            &params.chain,
            &address,
            &state.config.keymaster_url,
            &state.config.keymaster_token,
            &state.http_client,
        )
        .await
    {
        Ok(positions) => {
            let count = positions.len();
            Json(serde_json::json!({
                "wallet": params.wallet,
                "address": address,
                "chain": params.chain,
                "positions": positions,
                "count": count,
            }))
        }
        Err(e) => Json(serde_json::json!({
            "positions": [],
            "error": e.to_string(),
        })),
    }
}

//─── full yield analysis ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AnalyzeRequest {
    wallet: String,
    chain: String,
}

async fn yield_analyze(
    State(state): State<AppState>,
    Json(body): Json<AnalyzeRequest>,
) -> Json<serde_json::Value> {
    //resolve wallet label/id to address
    let address = match aave::resolve_wallet_address(
        &state.http_client,
        &state.config.keymaster_url,
        &state.config.keymaster_token,
        &body.wallet,
        &body.chain,
    )
    .await
    {
        Ok(addr) => addr,
        Err(e) => {
            return Json(serde_json::json!({
                "status": "error",
                "error": format!("wallet resolution failed: {e}"),
            }));
        }
    };

    match state
        .yield_agent
        .analyze_live(&body.chain, &address)
        .await
    {
        Ok(analysis) => Json(serde_json::json!({
            "status": "ok",
            "wallet": body.wallet,
            "address": address,
            "chain": body.chain,
            "analysis": analysis,
        })),
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/yield/pools", get(yield_pools))
        .route("/yield/positions", get(yield_positions))
        .route("/yield/analyze", post(yield_analyze))
        .route("/yield/protocols", get(yield_protocols))
        .route("/yield/start", post(yield_start))
        .route("/yield/stop/:wallet_id", post(yield_stop))
        .route("/yield/status/:wallet_id", get(yield_status))
        .route("/yield/history/:wallet_id", get(yield_history))
}
