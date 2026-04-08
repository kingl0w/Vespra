use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use redis::AsyncCommands;
use uuid::Uuid;

use std::collections::HashMap;

use crate::agents::AgentClient;
use crate::types::goals::{CreateGoalRequest, GoalSpec, GoalStatus, GoalStrategy};

use super::AppState;

/// Default max concurrent running goals. Override with VESPRA_MAX_CONCURRENT_GOALS.
fn max_concurrent_goals() -> usize {
    std::env::var("VESPRA_MAX_CONCURRENT_GOALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

// ── Redis key helpers ───────────────────────────────────────────

fn goal_key(id: &Uuid) -> String {
    format!("vespra:goal:{id}")
}

const GOALS_INDEX_KEY: &str = "vespra:goals:index";

// ── Redis storage ───────────────────────────────────────────────

pub async fn save_goal(redis: &redis::Client, goal: &GoalSpec) -> anyhow::Result<()> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let json = serde_json::to_string(goal)?;
    conn.set::<_, _, ()>(goal_key(&goal.id), &json).await?;
    conn.sadd::<_, _, ()>(GOALS_INDEX_KEY, goal.id.to_string()).await?;
    Ok(())
}

pub async fn get_goal(redis: &redis::Client, id: Uuid) -> anyhow::Result<GoalSpec> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let raw: Option<String> = conn.get(goal_key(&id)).await?;
    let raw = raw.ok_or_else(|| anyhow::anyhow!("goal not found: {id}"))?;
    Ok(serde_json::from_str(&raw)?)
}

/// Public alias for use from main.rs boot resume.
pub async fn list_goals_all(redis: &redis::Client) -> anyhow::Result<Vec<GoalSpec>> {
    list_goals(redis).await
}

async fn list_goals(redis: &redis::Client) -> anyhow::Result<Vec<GoalSpec>> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let ids: Vec<String> = conn.smembers(GOALS_INDEX_KEY).await?;
    let mut goals = Vec::with_capacity(ids.len());
    for id_str in ids {
        let key = format!("vespra:goal:{id_str}");
        if let Ok(Some(raw)) = conn.get::<_, Option<String>>(&key).await {
            if let Ok(g) = serde_json::from_str::<GoalSpec>(&raw) {
                goals.push(g);
            }
        }
    }
    goals.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(goals)
}

async fn update_goal_status(
    redis: &redis::Client,
    id: Uuid,
    status: GoalStatus,
) -> anyhow::Result<GoalSpec> {
    let mut goal = get_goal(redis, id).await?;
    goal.status = status;
    goal.updated_at = Utc::now();
    save_goal(redis, &goal).await?;
    Ok(goal)
}

pub async fn update_goal_step(redis: &redis::Client, id: Uuid, step: &str) -> anyhow::Result<()> {
    let mut goal = get_goal(redis, id).await?;
    goal.current_step = step.to_string();
    goal.updated_at = Utc::now();
    save_goal(redis, &goal).await
}

pub async fn update_goal_pnl(redis: &redis::Client, id: Uuid, current_eth: f64) -> anyhow::Result<()> {
    let mut goal = get_goal(redis, id).await?;
    goal.current_eth = current_eth;
    goal.pnl_eth = current_eth - goal.entry_eth;
    goal.pnl_pct = if goal.entry_eth > 0.0 {
        (goal.pnl_eth / goal.entry_eth) * 100.0
    } else {
        0.0
    };
    goal.updated_at = Utc::now();
    save_goal(redis, &goal).await
}

/// List goals filtered by status.
pub async fn list_goals_by_status(
    redis: &redis::Client,
    status: GoalStatus,
) -> anyhow::Result<Vec<GoalSpec>> {
    let all = list_goals(redis).await?;
    Ok(all.into_iter().filter(|g| g.status == status).collect())
}

// ── Wallet label → UUID resolution ─────────────────────────────

/// Resolved wallet info pulled from Keymaster. The address is needed for the
/// pre-creation balance check (sprint 7); the id is what the goal store
/// persists for runner lookups.
pub struct ResolvedWallet {
    pub id: String,
    pub address: String,
}

/// Look up a wallet by its label via Keymaster's `/wallets` endpoint.
/// Returns the wallet's UUID and on-chain address on a case-insensitive label
/// match.
async fn resolve_wallet_info(
    keymaster_url: &str,
    keymaster_token: &str,
    wallet_label: &str,
) -> anyhow::Result<ResolvedWallet> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let resp = client
        .get(format!("{keymaster_url}/wallets"))
        .header("Authorization", format!("Bearer {keymaster_token}"))
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("keymaster /wallets returned {}", resp.status());
    }

    let data: serde_json::Value = resp.json().await?;
    let wallets = data
        .as_array()
        .or_else(|| data["wallets"].as_array())
        .ok_or_else(|| anyhow::anyhow!("keymaster /wallets returned unexpected shape"))?;

    let label_lower = wallet_label.to_lowercase();
    for w in wallets {
        let label = w["label"].as_str().unwrap_or("");
        if label.to_lowercase() == label_lower {
            let id = w["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("wallet {label} has no id field"))?
                .to_string();
            let address = w["address"].as_str().unwrap_or("").to_string();
            return Ok(ResolvedWallet { id, address });
        }
    }

    anyhow::bail!("no wallet with label '{wallet_label}' found in Keymaster")
}

// ── Wallet safeguards (sprint 7) ───────────────────────────────

/// Reserved gas budget per wallet — capital-eth requests must leave at least
/// this much native ETH for transaction fees.
pub const GAS_RESERVE_ETH: f64 = 0.005;

/// Returns the id of an existing in-flight goal owned by `wallet_label`, if
/// any. "In-flight" means a goal whose status is `Pending`, `Running`, or
/// `Paused` — anything Cancelled, Completed, or Failed is treated as freed.
/// The check is case-insensitive on the label, matching how Keymaster
/// resolves labels.
pub async fn wallet_has_active_goal(
    redis: &redis::Client,
    wallet_label: &str,
) -> Option<Uuid> {
    let goals = list_goals(redis).await.ok()?;
    let label_lower = wallet_label.to_lowercase();
    goals.into_iter().find_map(|g| {
        let active = matches!(
            g.status,
            GoalStatus::Pending | GoalStatus::Running | GoalStatus::Paused
        );
        if active && g.wallet_label.to_lowercase() == label_lower {
            Some(g.id)
        } else {
            None
        }
    })
}

/// Fetch the live ETH balance for `address` over JSON-RPC. Mirrors the
/// `eth_getBalance` pattern in `data/wallet.rs` but is standalone so the goal
/// route can call it without dragging in the WalletFetcher's caching layer.
async fn fetch_wallet_balance_eth(rpc_url: &str, address: &str) -> anyhow::Result<f64> {
    if rpc_url.is_empty() {
        anyhow::bail!("no rpc_url configured");
    }
    if address.is_empty() {
        anyhow::bail!("wallet address is empty");
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [address, "latest"],
        "id": 1
    });

    let resp: serde_json::Value = client
        .post(rpc_url)
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;

    let hex_str = resp
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing result in eth_getBalance response"))?;
    let hex_str = hex_str.trim_start_matches("0x");
    let wei = u128::from_str_radix(hex_str, 16)?;
    Ok(wei as f64 / 1e18)
}

// ── Coordinator LLM parsing ─────────────────────────────────────

const GOAL_PARSE_PROMPT: &str = "\
You are Vespra GoalEngine. Parse the user's natural-language goal into a structured JSON object.\n\
Extract these fields from the text:\n\
- capital_eth: float (amount of ETH to use)\n\
- target_gain_pct: float (target gain percentage, default 10.0)\n\
- stop_loss_pct: float (stop loss percentage, default 5.0)\n\
- strategy: one of \"compound\", \"yield_rotate\", \"snipe\", \"adaptive\" (default \"adaptive\")\n\
- chain: string (e.g. \"base_sepolia\", \"base\", \"arbitrum\", default \"base_sepolia\")\n\n\
Respond with ONLY valid JSON matching this schema, no prose:\n\
{\"capital_eth\": float, \"target_gain_pct\": float, \"stop_loss_pct\": float, \"strategy\": string, \"chain\": string}";

async fn parse_goal_via_llm(
    llm: &dyn AgentClient,
    raw_goal: &str,
    wallet_label: &str,
) -> anyhow::Result<GoalSpec> {
    let task = format!("Parse this goal:\n\n\"{raw_goal}\"\n\nWallet: {wallet_label}");
    let raw = llm.call(GOAL_PARSE_PROMPT, &task).await?;

    // VES-87: never echo LLM output to clients — it can leak prompt content
    // or upstream context. Log the raw output at debug for operator triage
    // and return a generic error from the route handler.
    let val: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        let snippet: String = raw.chars().take(500).collect();
        tracing::debug!(
            "[goals] LLM returned invalid JSON: {e} | raw output: {snippet}"
        );
        anyhow::anyhow!("LLM returned invalid JSON")
    })?;

    let now = Utc::now();
    let capital = val
        .get("capital_eth")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let strategy_str = val
        .get("strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("adaptive");
    let strategy = match strategy_str {
        "compound" => GoalStrategy::Compound,
        "yield_rotate" => GoalStrategy::YieldRotate,
        "snipe" => GoalStrategy::Snipe,
        _ => GoalStrategy::Adaptive,
    };

    Ok(GoalSpec {
        id: Uuid::new_v4(),
        raw_goal: raw_goal.to_string(),
        wallet_label: wallet_label.to_string(),
        wallet_id: None, // resolved by caller after this returns
        chain: val
            .get("chain")
            .and_then(|v| v.as_str())
            .unwrap_or("base_sepolia")
            .to_string(),
        capital_eth: capital,
        target_gain_pct: val
            .get("target_gain_pct")
            .and_then(|v| v.as_f64())
            .unwrap_or(10.0),
        stop_loss_pct: val
            .get("stop_loss_pct")
            .and_then(|v| v.as_f64())
            .unwrap_or(5.0),
        strategy,
        status: GoalStatus::Pending,
        cycles: 0,
        current_step: "SCOUTING".to_string(),
        entry_eth: capital,
        current_eth: capital,
        pnl_eth: 0.0,
        pnl_pct: 0.0,
        token_address: None,
        token_amount_held: None,
        resolved_wallet_uuid: None,
        created_at: now,
        updated_at: now,
        error: None,
    })
}

// ── Route handlers ──────────────────────────────────────────────

async fn create_goal(
    State(state): State<AppState>,
    Json(body): Json<CreateGoalRequest>,
) -> impl IntoResponse {
    if body.raw_goal.trim().len() < 10 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "error": "raw_goal must be at least 10 characters"
            })),
        );
    }
    if body.wallet_label.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "error": "wallet_label is required"
            })),
        );
    }

    // ── Sprint 7: serialize the wallet-active-goal check + insert ─
    // Holding this lock for the duration of the check, the LLM parse, the
    // balance check, and the save guarantees two simultaneous submissions
    // for the same wallet can't both pass `wallet_has_active_goal`.
    let _create_guard = state.goal_creation_lock.lock().await;

    // ── Constraint: one active goal per wallet ────────────────────
    if let Some(existing_id) =
        wallet_has_active_goal(&state.redis, &body.wallet_label).await
    {
        tracing::info!(
            "goal rejected — wallet {} already has active goal {}",
            body.wallet_label,
            existing_id
        );
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "status": "error",
                "error": format!(
                    "wallet {} already has an active goal — cancel or wait for it to complete before submitting a new one",
                    body.wallet_label
                )
            })),
        );
    }

    // ── Constraint: max concurrent goals ──────────────────────────
    if let Ok(running) = list_goals_by_status(&state.redis, GoalStatus::Running).await {
        let max = max_concurrent_goals();
        if running.len() >= max {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "status": "error",
                    "error": format!(
                        "Maximum concurrent goals ({max}) reached. Cancel or complete a goal first."
                    )
                })),
            );
        }
    }

    // Resolve wallet_label → wallet info (UUID + address) via Keymaster
    // before doing any LLM work, so callers get a fast 400 if the label
    // doesn't exist. The address is needed for the live balance check below.
    let resolved = match resolve_wallet_info(
        &state.config.keymaster_url,
        &state.config.keymaster_token,
        &body.wallet_label,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "error": format!("wallet_label resolution failed: {e}")
                })),
            );
        }
    };

    let mut goal = match parse_goal_via_llm(
        state.llm.as_ref(),
        &body.raw_goal,
        &body.wallet_label,
    )
    .await
    {
        Ok(g) => g,
        Err(e) => {
            // VES-87: log the underlying error for operator triage but never
            // surface it to the client — the LLM raw output may contain
            // prompt content or upstream context.
            tracing::warn!("[goals] parse_goal_via_llm failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "error": "goal processing failed — please retry"
                })),
            );
        }
    };
    // VES-104: reject 0/negative capital up front. Without this guard the LLM
    // returning a string, omitting the field, or producing 0 silently creates
    // a goal that can never make progress.
    if !(goal.capital_eth > 0.0) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "error": "capital_eth must be greater than zero"
            })),
        );
    }

    // ── Sprint 7: live balance check on the target chain ──────────
    // A bad RPC must NOT block goal creation — fall through with a warning
    // so a flaky provider can't permanently brick submissions.
    let rpc_url = state
        .chain_registry
        .get(&goal.chain)
        .map(|c| c.rpc_url.clone())
        .unwrap_or_default();
    if rpc_url.is_empty() {
        tracing::warn!(
            "balance check skipped for wallet {} — no rpc_url for chain '{}'",
            body.wallet_label,
            goal.chain
        );
    } else {
        match fetch_wallet_balance_eth(&rpc_url, &resolved.address).await {
            Ok(balance_eth) => {
                if goal.capital_eth > (balance_eth - GAS_RESERVE_ETH) {
                    tracing::warn!(
                        "goal rejected — insufficient balance for wallet {}: requested={} available={}",
                        body.wallet_label,
                        goal.capital_eth,
                        balance_eth
                    );
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "status": "error",
                            "error": format!(
                                "insufficient wallet balance — requested {:.4} ETH but wallet holds {:.4} ETH (reserve: {:.3} ETH)",
                                goal.capital_eth, balance_eth, GAS_RESERVE_ETH
                            )
                        })),
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "balance check failed for wallet {} — proceeding without confirmation: {e}",
                    body.wallet_label
                );
            }
        }
    }

    goal.wallet_id = Some(resolved.id);

    if let Err(e) = save_goal(&state.redis, &goal).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "error": format!("Failed to save goal: {e}")
            })),
        );
    }

    // Set status to Running and spawn the GoalRunner
    goal.status = GoalStatus::Running;
    if let Err(e) = save_goal(&state.redis, &goal).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "error": format!("Failed to update goal status: {e}")
            })),
        );
    }

    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let goal_id = goal.id;
    let deps = state.goal_runner_deps.clone();
    let handle = tokio::spawn(async move {
        crate::goal_runner::run_goal(goal_id, cancel_rx, deps).await;
    });

    {
        let mut runners = state.goal_runners.lock().await;
        runners.insert(goal_id, handle);
    }
    {
        let mut txs = state.goal_cancel_txs.lock().await;
        txs.insert(goal_id, cancel_tx);
    }

    tracing::info!(
        "goal created and runner spawned id={} strategy={:?} capital={}",
        goal.id,
        goal.strategy,
        goal.capital_eth
    );

    // VES-103: never silently 201 with an empty body — the client needs the
    // goal_id. Surface a 500 if serialization fails so they can retry.
    match serde_json::to_value(&goal) {
        Ok(v) => (StatusCode::CREATED, Json(v)),
        Err(e) => {
            tracing::error!("failed to serialize created goal {}: {e}", goal.id);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "status": "error",
                    "error": "failed to serialize created goal — please retry"
                })),
            )
        }
    }
}

async fn list_goals_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    match list_goals(&state.redis).await {
        Ok(goals) => Json(serde_json::json!({ "goals": goals, "count": goals.len() })),
        Err(e) => Json(serde_json::json!({ "status": "error", "error": e.to_string() })),
    }
}

async fn get_goal_handler(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match get_goal(&state.redis, id).await {
        Ok(goal) => (
            StatusCode::OK,
            Json(serde_json::to_value(&goal).unwrap_or_default()),
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "status": "error", "error": format!("goal not found: {id}") })),
        ),
    }
}

async fn cancel_goal(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    // Send cancel signal to runner
    {
        let txs = state.goal_cancel_txs.lock().await;
        if let Some(tx) = txs.get(&id) {
            let _ = tx.send(true);
        }
    }

    match update_goal_status(&state.redis, id, GoalStatus::Cancelled).await {
        Ok(goal) => (
            StatusCode::OK,
            Json(serde_json::to_value(&goal).unwrap_or_default()),
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "status": "error", "error": format!("goal not found: {id}") })),
        ),
    }
}

async fn pause_goal(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match update_goal_status(&state.redis, id, GoalStatus::Paused).await {
        Ok(goal) => (
            StatusCode::OK,
            Json(serde_json::to_value(&goal).unwrap_or_default()),
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "status": "error", "error": format!("goal not found: {id}") })),
        ),
    }
}

async fn resume_goal(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match update_goal_status(&state.redis, id, GoalStatus::Running).await {
        Ok(goal) => (
            StatusCode::OK,
            Json(serde_json::to_value(&goal).unwrap_or_default()),
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "status": "error", "error": format!("goal not found: {id}") })),
        ),
    }
}

async fn portfolio(State(state): State<AppState>) -> Json<serde_json::Value> {
    let goals = list_goals(&state.redis).await.unwrap_or_default();

    let mut by_status: HashMap<String, u32> = HashMap::new();
    let mut by_strategy: HashMap<String, u32> = HashMap::new();
    let mut total_capital = 0.0_f64;
    let mut total_current = 0.0_f64;
    let mut running_count = 0u32;

    for g in &goals {
        *by_status
            .entry(format!("{:?}", g.status))
            .or_default() += 1;
        *by_strategy
            .entry(format!("{:?}", g.strategy))
            .or_default() += 1;

        if g.status == GoalStatus::Running {
            running_count += 1;
            total_capital += g.entry_eth;
            total_current += g.current_eth;
        }
    }

    let total_pnl = total_current - total_capital;
    let total_pnl_pct = if total_capital > 0.0 {
        (total_pnl / total_capital) * 100.0
    } else {
        0.0
    };

    Json(serde_json::json!({
        "total_goals_running": running_count,
        "total_capital_eth": total_capital,
        "total_current_eth": total_current,
        "total_pnl_eth": total_pnl,
        "total_pnl_pct": total_pnl_pct,
        "goals_by_strategy": by_strategy,
        "goals_by_status": by_status,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/goals", post(create_goal).get(list_goals_handler))
        .route("/goals/portfolio", get(portfolio))
        .route("/goals/:id", get(get_goal_handler))
        .route("/goals/:id/cancel", post(cancel_goal))
        .route("/goals/:id/pause", post(pause_goal))
        .route("/goals/:id/resume", post(resume_goal))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_goal(wallet: &str, status: GoalStatus, strategy: GoalStrategy, capital: f64, current: f64) -> GoalSpec {
        let now = Utc::now();
        GoalSpec {
            id: Uuid::new_v4(),
            raw_goal: "test goal".into(),
            wallet_label: wallet.into(),
            wallet_id: None,
            chain: "base".into(),
            capital_eth: capital,
            target_gain_pct: 10.0,
            stop_loss_pct: 5.0,
            strategy,
            status,
            cycles: 0,
            current_step: "MONITORING".into(),
            entry_eth: capital,
            current_eth: current,
            pnl_eth: current - capital,
            pnl_pct: if capital > 0.0 { ((current - capital) / capital) * 100.0 } else { 0.0 },
            token_address: None,
            token_amount_held: None,
            resolved_wallet_uuid: None,
            created_at: now,
            updated_at: now,
            error: None,
        }
    }

    #[test]
    fn test_wallet_uniqueness_gate() {
        // Simulate: two running goals, check if wallet "safe" has an active goal
        let goals = vec![
            make_goal("safe", GoalStatus::Running, GoalStrategy::Adaptive, 0.1, 0.1),
            make_goal("hot", GoalStatus::Completed, GoalStrategy::Snipe, 0.05, 0.06),
        ];

        let running: Vec<_> = goals.iter().filter(|g| g.status == GoalStatus::Running).collect();

        // "safe" wallet has a running goal — should be rejected
        let conflict = running.iter().find(|g| g.wallet_label == "safe");
        assert!(conflict.is_some(), "should find active goal for wallet 'safe'");

        // "hot" wallet has no running goal — should be allowed
        let no_conflict = running.iter().find(|g| g.wallet_label == "hot");
        assert!(no_conflict.is_none(), "wallet 'hot' has no running goal");

        // "new" wallet has no goals at all — should be allowed
        let new_wallet = running.iter().find(|g| g.wallet_label == "new");
        assert!(new_wallet.is_none());
    }

    #[test]
    fn test_max_concurrent_limit() {
        let mut goals = Vec::new();
        for i in 0..5 {
            goals.push(make_goal(
                &format!("wallet-{i}"),
                GoalStatus::Running,
                GoalStrategy::Adaptive,
                0.1,
                0.1,
            ));
        }

        let running_count = goals.iter().filter(|g| g.status == GoalStatus::Running).count();
        let max = 5;

        // At max — should reject
        assert!(running_count >= max, "should be at max capacity");

        // Below max — should allow
        goals[4].status = GoalStatus::Completed;
        let running_count = goals.iter().filter(|g| g.status == GoalStatus::Running).count();
        assert!(running_count < max, "should have room after completing one");
    }

    #[test]
    fn test_portfolio_pnl_weighted_average() {
        let goals = vec![
            make_goal("w1", GoalStatus::Running, GoalStrategy::Compound, 1.0, 1.1),   // +10%
            make_goal("w2", GoalStatus::Running, GoalStrategy::YieldRotate, 2.0, 1.8), // -10%
            make_goal("w3", GoalStatus::Completed, GoalStrategy::Snipe, 0.5, 0.6),     // not counted
        ];

        let running: Vec<_> = goals.iter().filter(|g| g.status == GoalStatus::Running).collect();
        let total_capital: f64 = running.iter().map(|g| g.entry_eth).sum();
        let total_current: f64 = running.iter().map(|g| g.current_eth).sum();
        let total_pnl = total_current - total_capital;
        let total_pnl_pct = if total_capital > 0.0 {
            (total_pnl / total_capital) * 100.0
        } else {
            0.0
        };

        // capital = 1.0 + 2.0 = 3.0, current = 1.1 + 1.8 = 2.9
        // pnl = -0.1, pnl_pct = -0.1/3.0 * 100 = -3.33%
        assert!((total_capital - 3.0).abs() < 1e-10);
        assert!((total_current - 2.9).abs() < 1e-10);
        assert!((total_pnl - (-0.1)).abs() < 1e-10);
        assert!((total_pnl_pct - (-100.0 / 30.0)).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_redis_save_get_list() {
        // Only runs when Redis is available
        let client = match redis::Client::open("redis://127.0.0.1:6379") {
            Ok(c) => c,
            Err(_) => return, // skip if no redis
        };
        if client.get_multiplexed_async_connection().await.is_err() {
            return; // skip if no redis
        }

        let now = Utc::now();
        let goal = GoalSpec {
            id: Uuid::new_v4(),
            raw_goal: "Test goal for redis".to_string(),
            wallet_label: "test-wallet".to_string(),
            wallet_id: None,
            chain: "base_sepolia".to_string(),
            capital_eth: 0.1,
            target_gain_pct: 15.0,
            stop_loss_pct: 5.0,
            strategy: GoalStrategy::Adaptive,
            status: GoalStatus::Pending,
            cycles: 0,
            current_step: "SCOUTING".to_string(),
            entry_eth: 0.1,
            current_eth: 0.1,
            pnl_eth: 0.0,
            pnl_pct: 0.0,
            token_address: None,
            token_amount_held: None,
            resolved_wallet_uuid: None,
            created_at: now,
            updated_at: now,
            error: None,
        };

        // save
        save_goal(&client, &goal).await.expect("save_goal");

        // get
        let fetched = get_goal(&client, goal.id).await.expect("get_goal");
        assert_eq!(fetched.id, goal.id);
        assert_eq!(fetched.capital_eth, 0.1);

        // list
        let all = list_goals(&client).await.expect("list_goals");
        assert!(all.iter().any(|g| g.id == goal.id));

        // update status
        let updated = update_goal_status(&client, goal.id, GoalStatus::Cancelled)
            .await
            .expect("update_goal_status");
        assert_eq!(updated.status, GoalStatus::Cancelled);

        // cleanup
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::cmd("DEL")
            .arg(goal_key(&goal.id))
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SREM")
            .arg(GOALS_INDEX_KEY)
            .arg(goal.id.to_string())
            .query_async(&mut conn)
            .await
            .unwrap();
    }
}
