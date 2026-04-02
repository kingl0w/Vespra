use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use redis::AsyncCommands;

use super::AppState;

async fn get_config(State(state): State<AppState>) -> Json<serde_json::Value> {
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
        "default_custody": cfg.default_custody,
        "trader_max_slippage_pct": cfg.trader_max_slippage_pct,
        "volatility_gate_threshold": cfg.volatility_gate_threshold,
        "rpc_url_override": cfg.rpc_url_override,
    }))
}

async fn patch_config(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    // Persist config overrides to Redis — applied on next service restart
    match redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await {
        Ok(mut conn) => {
            // Merge incoming fields into existing overrides
            let existing: serde_json::Value = conn
                .get::<_, Option<String>>("vespra:config_overrides")
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::json!({}));

            let mut merged = existing.as_object().cloned().unwrap_or_default();
            if let Some(obj) = body.as_object() {
                for (k, v) in obj {
                    merged.insert(k.clone(), v.clone());
                }
            }

            let json = serde_json::to_string(&merged).unwrap_or_default();
            let _: Result<(), _> = conn
                .set::<_, _, ()>("vespra:config_overrides", &json)
                .await;

            Json(serde_json::json!({
                "status": "updated",
                "persisted_keys": merged.keys().collect::<Vec<_>>(),
                "note": "overrides take effect on next service restart",
            }))
        }
        Err(_) => Json(serde_json::json!({
            "status": "error",
            "error": "redis_unavailable",
        })),
    }
}

async fn get_rate_limits(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.route_limiters.config_json())
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/config", get(get_config).patch(patch_config))
        .route("/api/rate-limits", get(get_rate_limits))
}
