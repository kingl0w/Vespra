use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Wallet not found: {0}")]
    WalletNotFound(String),

    #[error("Chain not configured: {0}")]
    ChainNotConfigured(String),

    #[error("Encryption error: {0}")]
    Encryption(String),

    #[error("Decryption error: {0}")]
    Decryption(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("RPC error: {0}")]
    Rpc(String),

    #[error("Transaction error: {0}")]
    Transaction(String),

    #[error("Invalid request: {0}")]
    BadRequest(String),

    #[error("Invalid hex for field '{field}': {detail}")]
    InvalidHex { field: String, detail: String },

    #[error("Wallet cap exceeded: balance {balance}, cap {cap}")]
    CapExceeded { balance: String, cap: String },

    ///ves-90: cap_wei field on a wallet record could not be parsed as u128.
    ///this is an integrity error, not a client error — refuse the tx.
    #[error("Wallet cap field corrupted (raw='{0}')")]
    CapCorrupt(String),

    ///ves-105: total_sent on a wallet has somehow exceeded its cap. treat
    ///the wallet as quarantined until an operator inspects the keystore.
    #[error("Wallet spend cap integrity error: address={address} total_sent={total_sent} cap={cap}")]
    CapIntegrity { address: String, total_sent: String, cap: String },

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Kill switch active — signing disabled")]
    KillSwitchActive,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        //sanitized messages — never echo user input back
        let (status, message): (StatusCode, String) = match &self {
            AppError::WalletNotFound(_) => (StatusCode::NOT_FOUND, "Wallet not found".into()),
            //ves-109: chain-not-configured is a server config issue, not a
            //client mistake — return 503 so operators see the right signal.
            AppError::ChainNotConfigured(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "chain not configured on this Keymaster instance".into(),
            ),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "Invalid request".into()),
            //ves-112: surface the field name + decode detail verbatim so the
            //operator can fix the request without grepping logs.
            AppError::InvalidHex { field, detail } => (
                StatusCode::BAD_REQUEST,
                format!(
                    "hex decode failed for field '{field}' — expected 0x-prefixed hex string ({detail})"
                ),
            ),
            AppError::CapExceeded { .. } => (StatusCode::FORBIDDEN, "Wallet cap exceeded".into()),
            AppError::CapCorrupt(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "wallet cap field corrupted — transaction rejected for safety".into(),
            ),
            AppError::CapIntegrity { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "wallet spend cap integrity error — contact operator to inspect wallet state".into(),
            ),
            AppError::Encryption(_) | AppError::Decryption(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Cryptographic operation failed".into())
            }
            AppError::Database(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error".into())
            }
            AppError::Rpc(detail) => (
                StatusCode::BAD_GATEWAY,
                format!("RPC error: {}", sanitize_error_detail(detail)),
            ),
            AppError::Transaction(detail) => (
                StatusCode::BAD_REQUEST,
                format!("Transaction failed: {}", sanitize_error_detail(detail)),
            ),
            AppError::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal error".into())
            }
            AppError::KillSwitchActive => (
                StatusCode::SERVICE_UNAVAILABLE,
                "kill switch active — signing disabled".into(),
            ),
        };

        tracing::error!("AppError: {self}");

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn kill_switch_active_returns_503() {
        let resp = AppError::KillSwitchActive.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = resp.into_body();
        let bytes = to_bytes(body, 1024).await.unwrap();
        let body_str = std::str::from_utf8(&bytes).unwrap();
        assert!(
            body_str.contains("kill switch active") && body_str.contains("signing disabled"),
            "expected clear error message, got: {body_str}"
        );
    }
}

fn sanitize_error_detail(s: &str) -> String {
    const MAX: usize = 200;
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.chars().count() > MAX {
        let truncated: String = cleaned.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        cleaned
    }
}
