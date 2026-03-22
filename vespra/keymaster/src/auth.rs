use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Json,
};
use serde_json::json;
use std::sync::Arc;

use crate::state::AppState;

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let token = &state.auth_token;

    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    match auth_header {
        Some(header) if header.starts_with("Bearer ") => {
            let provided = &header[7..];
            if provided == token {
                Ok(next.run(req).await)
            } else {
                tracing::warn!("Auth failed: invalid token");
                Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": "Invalid authentication token"})),
                ))
            }
        }
        _ => {
            tracing::warn!("Auth failed: missing or malformed Authorization header");
            Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Authorization: Bearer <token> required"})),
            ))
        }
    }
}
