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

/// Constant-time byte comparison so a wrong bearer token can't be recovered
/// via response-timing. `black_box` stops the optimizer short-circuiting the
/// fold. Length is not treated as secret (token length isn't the secret material).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

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
            if constant_time_eq(provided.as_bytes(), token.as_bytes()) {
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
