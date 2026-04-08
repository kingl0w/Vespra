//! REST routes for the backtesting engine.
//!
//! Persistence layout in Redis:
//! - `backtest:{id}` → JSON-encoded `BacktestResult` (no TTL — backtests are
//!   small and operators want them to stick around)
//! - `backtest:index` → JSON array of `BacktestSummary`, newest-first
//!
//! The index is a single key (rather than a sorted set) so the dashboard can
//! fetch the full list in one round trip without paging logic.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use redis::AsyncCommands;
use serde_json::json;
use uuid::Uuid;

use super::AppState;
use crate::backtest::runner::run_backtest;
use crate::backtest::types::{BacktestRequest, BacktestResult, BacktestSummary};

const BACKTEST_KEY_PREFIX: &str = "backtest:";
const BACKTEST_INDEX_KEY: &str = "backtest:index";

fn backtest_key(id: &Uuid) -> String {
    format!("{BACKTEST_KEY_PREFIX}{id}")
}

// ─── Persistence helpers ───────────────────────────────────────────────

async fn save_backtest(redis: &redis::Client, result: &BacktestResult) -> anyhow::Result<()> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let payload = serde_json::to_string(result)?;
    let _: () = conn.set(backtest_key(&result.id), payload).await?;

    // Maintain backtest:index — newest-first list of summaries.
    let raw: Option<String> = conn.get(BACKTEST_INDEX_KEY).await.unwrap_or(None);
    let mut index: Vec<BacktestSummary> = raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    index.insert(0, BacktestSummary::from(result));
    let serialized = serde_json::to_string(&index)?;
    let _: () = conn.set(BACKTEST_INDEX_KEY, serialized).await?;
    Ok(())
}

async fn load_backtest(redis: &redis::Client, id: Uuid) -> anyhow::Result<Option<BacktestResult>> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let raw: Option<String> = conn.get(backtest_key(&id)).await?;
    match raw {
        Some(s) => Ok(Some(serde_json::from_str(&s)?)),
        None => Ok(None),
    }
}

async fn load_index(redis: &redis::Client) -> anyhow::Result<Vec<BacktestSummary>> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let raw: Option<String> = conn.get(BACKTEST_INDEX_KEY).await?;
    let mut index: Vec<BacktestSummary> = raw
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    // Defensive: ensure newest-first even if a writer raced.
    index.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(index)
}

// ─── Handlers ──────────────────────────────────────────────────────────

async fn create_backtest(
    State(state): State<AppState>,
    Json(body): Json<BacktestRequest>,
) -> impl IntoResponse {
    if body.from_date > body.to_date {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "from_date must be on or before to_date"})),
        );
    }

    let result =
        match run_backtest(&body, state.historical_feed.clone(), state.llm.clone()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("[backtest] run failed: {e:#}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("backtest failed: {e}")})),
                );
            }
        };

    if let Err(e) = save_backtest(&state.redis, &result).await {
        tracing::error!("[backtest] persist failed for {}: {e:#}", result.id);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("persist failed: {e}")})),
        );
    }

    (
        StatusCode::CREATED,
        Json(serde_json::to_value(&result).unwrap_or_else(|_| json!({}))),
    )
}

async fn list_backtests(State(state): State<AppState>) -> impl IntoResponse {
    match load_index(&state.redis).await {
        Ok(index) => (
            StatusCode::OK,
            Json(serde_json::to_value(index).unwrap_or_else(|_| json!([]))),
        ),
        Err(e) => {
            tracing::error!("[backtest] index load failed: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("index load failed: {e}")})),
            )
        }
    }
}

async fn get_backtest(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match load_backtest(&state.redis, id).await {
        Ok(Some(r)) => (
            StatusCode::OK,
            Json(serde_json::to_value(r).unwrap_or_else(|_| json!({}))),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("backtest {id} not found")})),
        ),
        Err(e) => {
            tracing::error!("[backtest] load failed for {id}: {e:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("load failed: {e}")})),
            )
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/backtest", post(create_backtest))
        .route("/backtests", get(list_backtests))
        .route("/backtests/:id", get(get_backtest))
}
