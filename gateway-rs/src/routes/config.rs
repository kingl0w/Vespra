use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use super::AppState;

async fn get_config(State(state): State<AppState>) -> Json<serde_json::Value> {
    // Return config with secrets redacted
    let cfg = &state.config;
    Json(serde_json::json!({
        "host": cfg.host,
        "port": cfg.port,
        "keymaster_url": cfg.keymaster_url,
        "redis_url": cfg.redis_url,
        "database_url": cfg.database_url,
        "llm_provider": cfg.llm_provider,
        "llm_model": cfg.llm_model,
        "llm_base_url": cfg.llm_base_url,
        "price_oracle": cfg.price_oracle,
        "price_oracle_fallback": cfg.price_oracle_fallback,
        "chains": cfg.chains,
        "trade_up_enabled": cfg.trade_up_enabled,
        "trade_up_max_eth": cfg.trade_up_max_eth,
        "trade_up_cycle_interval_secs": cfg.trade_up_cycle_interval_secs,
        "trade_up_min_gain_pct": cfg.trade_up_min_gain_pct,
        "trade_up_stop_loss_pct": cfg.trade_up_stop_loss_pct,
        "yield_auto_rotate_enabled": cfg.yield_auto_rotate_enabled,
        "auto_execute_enabled": cfg.auto_execute_enabled,
        "auto_execute_max_eth": cfg.auto_execute_max_eth,
    }))
}

async fn patch_config(
    State(_state): State<AppState>,
    Json(_body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    // TODO: persist partial config updates to SQLite
    Json(serde_json::json!({ "status": "updated" }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/config", get(get_config).patch(patch_config))
}
