use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("config error: {0}")]
    Config(String),

    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("http client error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("chain error: {0}")]
    Chain(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("{0}")]
    Internal(String),
}

impl IntoResponse for GatewayError {
    fn into_response(self) -> Response {
        let status = match &self {
            GatewayError::NotFound(_) => StatusCode::NOT_FOUND,
            GatewayError::Config(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.to_string() });
        (status, axum::Json(body)).into_response()
    }
}
