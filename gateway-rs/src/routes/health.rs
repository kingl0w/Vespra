use axum::extract::State;
use axum::routing::get;
use axum::Json;
use axum::Router;

use super::AppState;

async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let available_chains: Vec<String> = state
        .chain_registry
        .available()
        .iter()
        .map(|c| c.name.clone())
        .collect();

    Json(serde_json::json!({
        "status": "ok",
        "service": "vespra-gateway-rs",
        "agents": ["scout", "risk", "trader", "sentinel", "executor"],
        "provider": state.config.llm_provider,
        "model": state.config.llm_model,
        "chains": available_chains,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health_check))
}
