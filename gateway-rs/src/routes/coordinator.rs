use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::orchestrator::coordinator;

use super::AppState;

//── get /coordinator/session ─────────────────────────────────────

async fn get_session(State(state): State<AppState>) -> Json<serde_json::Value> {
    let session = coordinator::load_session(&state.redis).await;
    Json(serde_json::json!({
        "entries": session.entries,
        "count": session.entries.len(),
        "started_at": session.started_at,
    }))
}

//── post /coordinator/orchestrate ────────────────────────────────

#[derive(Debug, Deserialize)]
struct OrchestrateRequest {
    intent: String,
    wallet: Option<String>,
    chain: Option<String>,
}

async fn orchestrate(
    State(state): State<AppState>,
    Json(body): Json<OrchestrateRequest>,
) -> Json<serde_json::Value> {
    let result = state
        .coordinator_orchestrator
        .orchestrate(
            &body.intent,
            body.wallet.as_deref(),
            body.chain.as_deref(),
        )
        .await;

    match result {
        Ok(orch_result) => {
            //if spawn_dag is set, fire-and-forget to nullboiler
            if let Some(ref dag_name) = orch_result.spawn_dag {
                let orch = state.coordinator_orchestrator.clone();
                let dag = dag_name.clone();
                let wallet = body.wallet.clone();
                let chain = body.chain.clone();
                tokio::spawn(async move {
                    orch.spawn_dag(&dag, wallet.as_deref(), chain.as_deref())
                        .await;
                });
            }

            Json(serde_json::json!({
                "status": "ok",
                "result": orch_result,
            }))
        }
        Err(e) => Json(serde_json::json!({
            "status": "error",
            "error": e.to_string(),
        })),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/coordinator/session", get(get_session))
        .route("/coordinator/orchestrate", post(orchestrate))
}
