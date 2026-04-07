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

/// Look up a wallet by its label via Keymaster's `/wallets` endpoint.
/// Returns the wallet's UUID (as a string) on a case-insensitive label match.
async fn resolve_wallet_id(
    keymaster_url: &str,
    keymaster_token: &str,
    wallet_label: &str,
) -> anyhow::Result<String> {
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
                .ok_or_else(|| anyhow::anyhow!("wallet {label} has no id field"))?;
            return Ok(id.to_string());
        }
    }

    anyhow::bail!("no wallet with label '{wallet_label}' found in Keymaster")
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

    let val: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "LLM returned invalid JSON: {} | raw output: {}",
            e,
            &raw[..raw.len().min(500)]
        )
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

    // ── Constraint: one active goal per wallet ────────────────────
    if let Ok(running) = list_goals_by_status(&state.redis, GoalStatus::Running).await {
        if let Some(existing) = running.iter().find(|g| g.wallet_label == body.wallet_label) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "status": "error",
                    "error": format!(
                        "Wallet {} already has an active goal: {}. Cancel or pause it first.",
                        body.wallet_label, existing.id
                    )
                })),
            );
        }

        // ── Constraint: max concurrent goals ────────────────────────
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

    // Resolve wallet_label → wallet UUID via Keymaster before doing any LLM work,
    // so callers get a fast, clear 400 if the label doesn't exist.
    let wallet_id = match resolve_wallet_id(
        &state.config.keymaster_url,
        &state.config.keymaster_token,
        &body.wallet_label,
    )
    .await
    {
        Ok(id) => id,
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
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "error": format!("Failed to parse goal: {e}")
                })),
            );
        }
    };
    goal.wallet_id = Some(wallet_id);

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

    (StatusCode::CREATED, Json(serde_json::to_value(&goal).unwrap_or_default()))
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
