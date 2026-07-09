//! Webhook worker endpoints that nullboiler dispatches DAG steps to.
//!
//! Each agent tag (scout/risk/trader/…) is a worker at
//! `POST /nullboiler/worker/:agent`. nullboiler sends the rendered step prompt
//! as `{message,text,session_key,session_id}` and expects a synchronous JSON
//! object with a `response` string (see nullboiler `worker_response.zig`).
//!
//! These run LLM reasoning ONLY — they produce analysis and PLANS. They never
//! call keymaster or sign anything: fund movement stays in the audited
//! in-process goal pipeline, never through a nullboiler step. The system
//! prompts below make that explicit ("plan, do not execute").

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};

use super::AppState;

/// System prompt per agent role. Planning/analysis framing, no execution.
fn system_prompt(agent: &str) -> &'static str {
    match agent {
        "scout" => "You are Scout, a DeFi opportunity finder. Given the request, identify the best \
                    matching pool/token (APY, TVL, liquidity, age) and report it concisely. Analysis only.",
        "risk" => "You are Risk, a safety evaluator. Grade the given opportunity LOW/MEDIUM/HIGH with a \
                   one-line justification. Flag low TVL, unverified tokens, or odd pairs. Analysis only.",
        "trader" => "You are Trader. Decide enter/hold/exit and produce a concise trade PLAN. \
                     Output a plan only — never execute, sign, or broadcast anything.",
        "executor" => "You are Executor. Describe the exact swap that WOULD be built (tokens, amounts, \
                       router, expected out) as a PLAN. Never sign or broadcast — output the plan only.",
        "sentinel" => "You are Sentinel, the position watchdog. Report the health of the described \
                       positions and whether any warrant exit for gain or stop-loss. Analysis only.",
        "yield" => "You are Yield. Given an opportunity, produce a deposit/rotation PLAN (amounts, \
                    from/to). Output a plan only — never execute.",
        "sniper" => "You are Sniper. Assess a newly launched pool and whether it passes entry criteria. \
                     Analysis only — output an assessment, never execute.",
        "coordinator" => "You are Coordinator. Summarize the inputs into a clear, concise report or plan \
                          for the operator. Prose only.",
        "launcher" => "You are Launcher. Given a token spec, describe the deployment PLAN (params, supply, \
                       chain). Output the plan only — never deploy or sign.",
        _ => "You are a Vespra analysis agent. Process the request and return a concise, useful result. \
              Analysis and planning only — never execute, sign, or broadcast anything.",
    }
}

/// `POST /nullboiler/worker/:agent` — run one DAG step as agent reasoning.
async fn worker_handler(
    State(state): State<AppState>,
    Path(agent): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    // nullboiler puts the rendered prompt in `message` (and duplicates it in `text`).
    let prompt = body
        .get("message")
        .or_else(|| body.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if prompt.trim().is_empty() {
        // 200 + error field → nullboiler marks the step failed (bad request, no retry help).
        return (
            StatusCode::OK,
            Json(serde_json::json!({"error": "empty prompt: expected `message` or `text`"})),
        );
    }

    match state.llm.call(system_prompt(&agent), prompt).await {
        Ok(text) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "ok", "response": text})),
        ),
        // transient LLM failure → non-2xx so nullboiler retries with backoff.
        Err(e) => {
            tracing::warn!("[nullboiler-worker:{agent}] llm call failed: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("llm call failed: {e}")})),
            )
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/nullboiler/worker/:agent", post(worker_handler))
}
