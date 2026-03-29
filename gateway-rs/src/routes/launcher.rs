use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::AppState;
use crate::orchestrator::launcher::TokenSpec;

#[derive(Debug, Deserialize)]
struct DeployRequest {
    name: String,
    symbol: String,
    supply: u64,
    #[serde(default = "default_decimals")]
    decimals: u8,
    chain: String,
    wallet_id: String,
    liquidity_eth: Option<f64>,
}

fn default_decimals() -> u8 { 18 }

async fn deploy_token(
    State(state): State<AppState>,
    Json(body): Json<DeployRequest>,
) -> Json<serde_json::Value> {
    let spec = TokenSpec {
        name: body.name,
        symbol: body.symbol,
        supply: body.supply,
        decimals: body.decimals,
        chain: body.chain,
        liquidity_eth: body.liquidity_eth,
    };

    let result = state.launcher_orchestrator.deploy(spec, body.wallet_id).await;
    Json(serde_json::to_value(&result).unwrap_or_default())
}

async fn list_contracts(State(state): State<AppState>) -> Json<serde_json::Value> {
    let contracts = state.launcher_orchestrator.list_contracts().await;
    Json(serde_json::json!({
        "count": contracts.len(),
        "contracts": contracts,
    }))
}

async fn get_contract(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    match state.launcher_orchestrator.get_contract(&address).await {
        Some(c) => Json(c),
        None => Json(serde_json::json!({
            "error": "contract_not_found",
        })),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/launcher/deploy", post(deploy_token))
        .route("/launcher/contracts", get(list_contracts))
        .route("/launcher/contracts/:address", get(get_contract))
}
