use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use uuid::Uuid;

use super::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/fees/summary", get(fee_summary))
        .route("/fees/goal/{id}", get(fee_by_goal))
}

async fn fee_summary(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(crate::fees::fee_summary(&state.redis).await)
}

async fn fee_by_goal(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match crate::fees::get_fee_record(&state.redis, id).await {
        Some(record) => (
            StatusCode::OK,
            Json(serde_json::to_value(&record).unwrap_or_default()),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("no fee record for goal {id}")
            })),
        )
            .into_response(),
    }
}
