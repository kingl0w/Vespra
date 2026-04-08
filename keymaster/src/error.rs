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

    #[error("Wallet cap exceeded: balance {balance}, cap {cap}")]
    CapExceeded { balance: String, cap: String },

    /// VES-90: cap_wei field on a wallet record could not be parsed as u128.
    /// This is an integrity error, not a client error — refuse the tx.
    #[error("Wallet cap field corrupted (raw='{0}')")]
    CapCorrupt(String),

    /// VES-105: total_sent on a wallet has somehow exceeded its cap. Treat
    /// the wallet as quarantined until an operator inspects the keystore.
    #[error("Wallet spend cap integrity error: address={address} total_sent={total_sent} cap={cap}")]
    CapIntegrity { address: String, total_sent: String, cap: String },

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Sanitized messages — never echo user input back
        let (status, message): (StatusCode, String) = match &self {
            AppError::WalletNotFound(_) => (StatusCode::NOT_FOUND, "Wallet not found".into()),
            AppError::ChainNotConfigured(_) => (StatusCode::BAD_REQUEST, "Chain not configured".into()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "Invalid request".into()),
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
            AppError::Rpc(_) => (StatusCode::BAD_GATEWAY, "RPC error".into()),
            AppError::Transaction(_) => (StatusCode::BAD_REQUEST, "Transaction failed".into()),
            AppError::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal error".into())
            }
        };

        tracing::error!("AppError: {self}");

        (status, Json(json!({ "error": message }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
