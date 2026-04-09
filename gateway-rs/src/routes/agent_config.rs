use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};

use crate::agent_config;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/config/agents", get(get_all_configs))
        .route(
            "/config/agents/{agent}",
            get(get_agent_config).patch(patch_agent_config),
        )
}

async fn get_all_configs(State(state): State<AppState>) -> impl IntoResponse {
    match agent_config::load_all_configs(&state.redis, &state.config).await {
        Ok(map) => Json(serde_json::Value::Object(map)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

async fn get_agent_config(
    State(state): State<AppState>,
    Path(agent): Path<String>,
) -> impl IntoResponse {
    if agent_config::known_fields(&agent).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("unknown agent '{agent}'") })),
        )
            .into_response();
    }

    match agent_config::load_agent_config(&state.redis, &agent, &state.config).await {
        Ok(config) => Json(config).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}

async fn patch_agent_config(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    Json(patch): Json<serde_json::Value>,
) -> impl IntoResponse {
    //validate agent name
    if agent_config::known_fields(&agent).is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("unknown agent '{agent}'") })),
        )
            .into_response();
    }

    //validate fields and ranges
    if let Err(err) = agent_config::validate_patch(&agent, &patch) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err })),
        )
            .into_response();
    }

    //load existing config (or defaults)
    let existing = match agent_config::load_agent_config(&state.redis, &agent, &state.config).await
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("{e}") })),
            )
                .into_response();
        }
    };

    //merge patch into existing
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    if let Some(obj) = patch.as_object() {
        for (k, v) in obj {
            merged.insert(k.clone(), v.clone());
        }
    }
    let merged_value = serde_json::Value::Object(merged);

    //save
    match agent_config::save_agent_config(&state.redis, &agent, &merged_value).await {
        Ok(()) => Json(serde_json::json!({
            "status": "updated",
            "agent": agent,
            "config": merged_value,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("{e}") })),
        )
            .into_response(),
    }
}
