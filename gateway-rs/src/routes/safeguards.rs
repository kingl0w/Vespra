use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};

use crate::safeguards;

use super::AppState;

async fn safeguards_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    let (current_hour, prior_hour) = match safeguards::tx_rate_counts_now(&state.redis).await {
        Ok(counts) => counts,
        Err(e) => {
            tracing::warn!("[safeguards] tx_rate_counts_now failed: {e}");
            (0, 0)
        }
    };

    let current_global_value_eth = safeguards::sum_global_wallet_value_eth(
        &state.goal_runner_deps.wallet_fetcher,
        &state.config.chains,
    )
    .await;

    Json(serde_json::json!({
        "global_cap_eth": state.config.max_global_wallet_value_eth,
        "current_global_value_eth": current_global_value_eth,
        "tx_limit_per_hour": state.config.max_tx_per_hour,
        "tx_count_current_hour": current_hour,
        "tx_count_prior_hour": prior_hour,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/safeguards/status", get(safeguards_status))
}
