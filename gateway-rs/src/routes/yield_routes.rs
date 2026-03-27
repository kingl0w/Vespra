use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use std::collections::BTreeSet;

use super::AppState;

#[derive(Debug, Deserialize)]
struct ProtocolsQuery {
    chain: Option<String>,
}

async fn yield_protocols(
    State(state): State<AppState>,
    Query(params): Query<ProtocolsQuery>,
) -> Json<serde_json::Value> {
    // Determine which chains to query
    let chains: Vec<String> = match &params.chain {
        Some(c) => vec![c.clone()],
        None => state.config.chains.clone(),
    };

    // Resolve chain names → defillama slugs via ChainRegistry
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

    // Fetch pools from DeFi Llama
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

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

pub fn router() -> Router<AppState> {
    Router::new().route("/yield/protocols", get(yield_protocols))
}
