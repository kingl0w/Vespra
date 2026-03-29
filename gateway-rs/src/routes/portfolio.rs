use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::AppState;
use crate::orchestrator::portfolio::{Allocation, CustodyMode, PortfolioSpec};

#[derive(Debug, Deserialize)]
struct DeployRequest {
    source_wallet_id: String,
    total_eth: f64,
    chain: String,
    #[serde(default)]
    custody: Option<String>,
    #[serde(default = "default_gas_reserve")]
    gas_reserve_per_wallet: f64,
    allocations: Vec<AllocationInput>,
}

fn default_gas_reserve() -> f64 {
    0.002
}

#[derive(Debug, Deserialize)]
struct AllocationInput {
    strategy: String,
    pct: f64,
    label: String,
}

async fn deploy_portfolio(
    State(state): State<AppState>,
    Json(body): Json<DeployRequest>,
) -> Json<serde_json::Value> {
    let custody = match body.custody.as_deref().unwrap_or(&state.config.default_custody) {
        "operator" => CustodyMode::Operator,
        _ => CustodyMode::Safe,
    };

    let spec = PortfolioSpec {
        source_wallet_id: body.source_wallet_id,
        total_eth: body.total_eth,
        chain: body.chain,
        custody,
        gas_reserve_per_wallet: body.gas_reserve_per_wallet,
        allocations: body
            .allocations
            .into_iter()
            .map(|a| Allocation {
                strategy: a.strategy,
                pct: a.pct,
                label: a.label,
            })
            .collect(),
    };

    let report = state.portfolio_orchestrator.deploy(spec).await;
    Json(serde_json::to_value(&report).unwrap_or_default())
}

async fn list_portfolios(State(state): State<AppState>) -> Json<serde_json::Value> {
    let portfolios = state.portfolio_orchestrator.list_portfolios().await;
    Json(serde_json::json!({
        "count": portfolios.len(),
        "portfolios": portfolios,
    }))
}

async fn get_portfolio(
    State(state): State<AppState>,
    Path(portfolio_id): Path<String>,
) -> Json<serde_json::Value> {
    match state
        .portfolio_orchestrator
        .portfolio_with_status(
            &portfolio_id,
            &state.trade_up_orchestrator,
            &state.yield_orchestrator,
        )
        .await
    {
        Some(v) => Json(v),
        None => Json(serde_json::json!({ "error": "portfolio_not_found" })),
    }
}

async fn exit_portfolio(
    State(state): State<AppState>,
    Path(portfolio_id): Path<String>,
) -> Json<serde_json::Value> {
    let result = state.portfolio_orchestrator.exit_portfolio(&portfolio_id).await;
    Json(result)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/portfolio/deploy", post(deploy_portfolio))
        .route("/portfolio", get(list_portfolios))
        .route("/portfolio/:portfolio_id", get(get_portfolio))
        .route("/portfolio/:portfolio_id/exit", post(exit_portfolio))
}
