use axum::routing::get;
use axum::{Json, Router};

use super::AppState;

async fn fee_summary() -> Json<serde_json::Value> {
    // TODO: fetch from FeeEngine
    Json(serde_json::json!({
        "total_fees_eth": 0.0,
        "pending_fees_eth": 0.0,
        "strategies": {},
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/fees/summary", get(fee_summary))
}
