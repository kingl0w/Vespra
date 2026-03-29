use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::routing::get;
use axum::Json;
use axum::Router;

use super::AppState;

/// GET /health — gateway-only health (existing behavior)
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
        "agents": ["scout", "risk", "trader", "sentinel", "executor", "yield", "sniper", "coordinator", "launcher"],
        "provider": state.config.llm_provider,
        "model": state.config.llm_model,
        "chains": available_chains,
        "kill_flag": state.kill_flag.load(Ordering::SeqCst),
    }))
}

/// GET /api/health — aggregated health from gateway + NullBoiler + Keymaster
/// (called from proxy router, path is "/health" under the /api nest)
pub async fn api_health_aggregate(State(state): State<AppState>) -> Json<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let services = [
        ("gateway", format!("http://127.0.0.1:{}/health", state.config.port)),
        ("boiler", format!("{}/health", state.config.nullboiler_url)),
        ("keymaster", format!("{}/health", state.config.keymaster_url)),
    ];

    let mut results = serde_json::Map::new();
    let mut all_ok = true;

    for (name, url) in &services {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let data: serde_json::Value = resp.json().await.unwrap_or(serde_json::json!({}));
                results.insert(
                    name.to_string(),
                    serde_json::json!({"status": "ok", "data": data}),
                );
            }
            Ok(resp) => {
                all_ok = false;
                results.insert(
                    name.to_string(),
                    serde_json::json!({"status": "down", "error": format!("HTTP {}", resp.status())}),
                );
            }
            Err(e) => {
                all_ok = false;
                results.insert(
                    name.to_string(),
                    serde_json::json!({"status": "down", "error": e.to_string()}),
                );
            }
        }
    }

    Json(serde_json::json!({
        "status": if all_ok { "ok" } else { "degraded" },
        "services": results,
    }))
}

/// GET /api/rate-limits
pub async fn api_rate_limits(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.rate_limiter.config_json())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health_check))
        .route("/api/health", get(api_health_aggregate))
        .route("/api/rate-limits", get(api_rate_limits))
}
