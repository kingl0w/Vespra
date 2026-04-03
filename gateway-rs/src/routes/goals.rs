use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use redis::AsyncCommands;
use uuid::Uuid;

use crate::agents::AgentClient;
use crate::types::goals::{CreateGoalRequest, GoalSpec, GoalStatus, GoalStrategy};

use super::AppState;

// ── Redis key helpers ───────────────────────────────────────────

fn goal_key(id: &Uuid) -> String {
    format!("vespra:goal:{id}")
}

const GOALS_INDEX_KEY: &str = "vespra:goals:index";

// ── Redis storage ───────────────────────────────────────────────

async fn save_goal(redis: &redis::Client, goal: &GoalSpec) -> anyhow::Result<()> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let json = serde_json::to_string(goal)?;
    conn.set::<_, _, ()>(goal_key(&goal.id), &json).await?;
    conn.sadd::<_, _, ()>(GOALS_INDEX_KEY, goal.id.to_string()).await?;
    Ok(())
}

async fn get_goal(redis: &redis::Client, id: Uuid) -> anyhow::Result<GoalSpec> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let raw: Option<String> = conn.get(goal_key(&id)).await?;
    let raw = raw.ok_or_else(|| anyhow::anyhow!("goal not found: {id}"))?;
    Ok(serde_json::from_str(&raw)?)
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

#[allow(dead_code)]
async fn update_goal_step(redis: &redis::Client, id: Uuid, step: &str) -> anyhow::Result<()> {
    let mut goal = get_goal(redis, id).await?;
    goal.current_step = step.to_string();
    goal.updated_at = Utc::now();
    save_goal(redis, &goal).await
}

#[allow(dead_code)]
async fn update_goal_pnl(redis: &redis::Client, id: Uuid, current_eth: f64) -> anyhow::Result<()> {
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

    let goal = match parse_goal_via_llm(
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

    if let Err(e) = save_goal(&state.redis, &goal).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "status": "error",
                "error": format!("Failed to save goal: {e}")
            })),
        );
    }

    tracing::info!(
        "goal created id={} strategy={:?} capital={}",
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

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/goals", post(create_goal).get(list_goals_handler))
        .route("/goals/{id}", get(get_goal_handler))
        .route("/goals/{id}/cancel", post(cancel_goal))
        .route("/goals/{id}/pause", post(pause_goal))
        .route("/goals/{id}/resume", post(resume_goal))
}

#[cfg(test)]
mod tests {
    use super::*;

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
