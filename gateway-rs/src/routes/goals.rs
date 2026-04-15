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

///default max concurrent running goals. override with vespra_max_concurrent_goals.
fn max_concurrent_goals() -> usize {
    std::env::var("VESPRA_MAX_CONCURRENT_GOALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
}

//── redis key helpers ───────────────────────────────────────────

fn goal_key(id: &Uuid) -> String {
    format!("vespra:goal:{id}")
}

const GOALS_INDEX_KEY: &str = "vespra:goals:index";

///30-day TTL applied to terminal goal records so they age out rather than
///accumulating forever. list_goals() already skips ids whose GET returns
///None, so expiry is self-cleaning on the read path; the startup sweep
///handles index-side orphans.
const TERMINAL_GOAL_TTL_SECS: i64 = 30 * 24 * 60 * 60;

fn is_terminal(status: &GoalStatus) -> bool {
    matches!(
        status,
        GoalStatus::Completed | GoalStatus::Failed | GoalStatus::Cancelled
    )
}

//── redis storage ───────────────────────────────────────────────

pub async fn save_goal(redis: &redis::Client, goal: &GoalSpec) -> anyhow::Result<()> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let json = serde_json::to_string(goal)?;
    let key = goal_key(&goal.id);
    conn.set::<_, _, ()>(&key, &json).await?;
    conn.sadd::<_, _, ()>(GOALS_INDEX_KEY, goal.id.to_string()).await?;
    if is_terminal(&goal.status) {
        //EXPIRE is idempotent — subsequent saves of an already-terminal goal
        //just reset the 30-day window. keys remain visible to list_goals()
        //until TTL fires, giving the dashboard a rolling-30-day history.
        conn.expire::<_, ()>(&key, TERMINAL_GOAL_TTL_SECS).await?;
    }
    Ok(())
}

pub async fn get_goal(redis: &redis::Client, id: Uuid) -> anyhow::Result<GoalSpec> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let raw: Option<String> = conn.get(goal_key(&id)).await?;
    let raw = raw.ok_or_else(|| anyhow::anyhow!("goal not found: {id}"))?;
    Ok(serde_json::from_str(&raw)?)
}

///public alias for use from main.rs boot resume.
pub async fn list_goals_all(redis: &redis::Client) -> anyhow::Result<Vec<GoalSpec>> {
    list_goals(redis).await
}

#[derive(Debug, Default, serde::Serialize)]
pub struct GoalSweepReport {
    pub scanned: usize,
    ///index entries whose underlying key was already gone (TTL fired or
    ///manually deleted). SREMed to keep the index truthful.
    pub orphan_index_entries_removed: usize,
    ///terminal goals older than the 30-day window — removed from both the
    ///index and the key store immediately.
    pub expired_terminal_purged: usize,
    ///terminal goals within the 30-day window that didn't have a TTL yet
    ///(pre-existing records from before this migration) — TTL applied.
    pub terminal_ttl_backfilled: usize,
}

///ves: startup sweep. walks the goals index once at boot and brings every
///entry into compliance with the 30-day terminal retention policy:
/// - drops index entries pointing at already-expired keys
/// - purges terminal goals older than 30d (key + index)
/// - backfills the TTL on terminal goals that pre-date this migration
///non-terminal goals are left untouched.
pub async fn sweep_terminal_goals(
    redis: &redis::Client,
) -> anyhow::Result<GoalSweepReport> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let ids: Vec<String> = conn.smembers(GOALS_INDEX_KEY).await?;
    let mut report = GoalSweepReport { scanned: ids.len(), ..Default::default() };
    let cutoff = Utc::now() - chrono::Duration::seconds(TERMINAL_GOAL_TTL_SECS);

    for id_str in ids {
        let key = format!("vespra:goal:{id_str}");
        let raw: Option<String> = conn.get(&key).await.unwrap_or(None);
        let Some(raw) = raw else {
            //key gone but index stale — clean the index pointer.
            let _: Result<(), _> = conn.srem::<_, _, ()>(GOALS_INDEX_KEY, &id_str).await;
            report.orphan_index_entries_removed += 1;
            continue;
        };
        let Ok(goal) = serde_json::from_str::<GoalSpec>(&raw) else {
            //undeserializable blob — leave it alone; surfacing via logs is
            //enough, and blind deletion risks losing a record we could
            //recover after a schema fix.
            tracing::warn!("[sweep] skip undeserializable goal {id_str}");
            continue;
        };
        if !is_terminal(&goal.status) {
            continue;
        }
        if goal.updated_at < cutoff {
            let _: Result<(), _> = conn.srem::<_, _, ()>(GOALS_INDEX_KEY, &id_str).await;
            let _: Result<(), _> = conn.del::<_, ()>(&key).await;
            report.expired_terminal_purged += 1;
        } else {
            //EXPIRE returns 0 if no TTL was applied (already set). we can't
            //distinguish "already had TTL" from "just set one" cheaply in
            //async redis, so count every backfill attempt — the value is
            //still useful as an upper bound on migration work.
            if conn.expire::<_, ()>(&key, TERMINAL_GOAL_TTL_SECS).await.is_ok() {
                report.terminal_ttl_backfilled += 1;
            }
        }
    }

    Ok(report)
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

///list goals filtered by status.
pub async fn list_goals_by_status(
    redis: &redis::Client,
    status: GoalStatus,
) -> anyhow::Result<Vec<GoalSpec>> {
    let all = list_goals(redis).await?;
    Ok(all.into_iter().filter(|g| g.status == status).collect())
}

//── wallet label → uuid resolution ─────────────────────────────

pub struct ResolvedWallet {
    pub id: String,
    pub address: String,
    pub cap_eth: Option<f64>,
}

async fn resolve_wallet_info(
    http_client: &reqwest::Client,
    keymaster_url: &str,
    keymaster_token: &str,
    wallet_label: &str,
) -> anyhow::Result<ResolvedWallet> {
    let resp = http_client
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
            //ves-fix: surface cap_wei so create_goal can clamp capital_eth
            //against the wallet's spend cap and avoid CapExceeded rejections.
            let cap_eth = w["cap_wei"]
                .as_str()
                .and_then(|s| s.parse::<u128>().ok())
                .map(|c| c as f64 / 1e18);
            return Ok(ResolvedWallet { id, address, cap_eth });
        }
    }

    anyhow::bail!("no wallet with label '{wallet_label}' found in Keymaster")
}

//── wallet safeguards (sprint 7) ───────────────────────────────

///reserved gas budget per wallet — capital-eth requests must leave at least
///this much native eth for transaction fees.
pub const GAS_RESERVE_ETH: f64 = 0.005;

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

async fn fetch_wallet_balance_eth(
    http_client: &reqwest::Client,
    rpc_url: &str,
    address: &str,
) -> anyhow::Result<f64> {
    if rpc_url.is_empty() {
        anyhow::bail!("no rpc_url configured");
    }
    if address.is_empty() {
        anyhow::bail!("wallet address is empty");
    }

    let client = http_client;

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

//── coordinator llm parsing ─────────────────────────────────────

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

fn classify_by_keyword(raw_goal: &str) -> Option<GoalStrategy> {
    let lower = raw_goal.to_lowercase();
    // yield_rotate keywords — checked first
    for kw in &["earn yield", "yield farm", "yield", "rotate"] {
        if lower.contains(kw) {
            return Some(GoalStrategy::YieldRotate);
        }
    }
    // compound keywords
    for kw in &["compound", "reinvest", "auto-compound"] {
        if lower.contains(kw) {
            return Some(GoalStrategy::Compound);
        }
    }
    // snipe keywords
    for kw in &["snipe", "new pool", "new pair", "launch"] {
        if lower.contains(kw) {
            return Some(GoalStrategy::Snipe);
        }
    }
    None
}

async fn parse_goal_via_llm(
    llm: &dyn AgentClient,
    raw_goal: &str,
    wallet_label: &str,
) -> anyhow::Result<GoalSpec> {
    let task = format!("Parse this goal:\n\n\"{raw_goal}\"\n\nWallet: {wallet_label}");
    let raw = llm.call(GOAL_PARSE_PROMPT, &task).await?;

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

    let strategy = if let Some(kw_strategy) = classify_by_keyword(raw_goal) {
        tracing::info!("[goals] strategy classified by keyword: {:?}", kw_strategy);
        kw_strategy
    } else {
        let strategy_str = val
            .get("strategy")
            .and_then(|v| v.as_str())
            .unwrap_or("adaptive");
        let llm_strategy = match strategy_str {
            "compound" => GoalStrategy::Compound,
            "yield_rotate" => GoalStrategy::YieldRotate,
            "snipe" => GoalStrategy::Snipe,
            _ => GoalStrategy::Adaptive,
        };
        tracing::info!("[goals] strategy classified by LLM: {:?}", llm_strategy);
        llm_strategy
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

//── route handlers ──────────────────────────────────────────────

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

    let _create_guard = state.goal_creation_lock.lock().await;

    //── constraint: one active goal per wallet ────────────────────
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

    //── constraint: max concurrent goals ──────────────────────────
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

    let resolved = match resolve_wallet_info(
        &state.http_client,
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

    if resolved.cap_eth == Some(0.0) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "error": "wallet cap is set to zero — no spending allowed on this wallet"
            })),
        );
    }

    // Extract the user's literal ETH amount from the raw goal text before the LLM
    // has a chance to pre-clamp it against wallet context.
    let user_stated_eth: Option<f64> = {
        let words: Vec<&str> = body.raw_goal.split_whitespace().collect();
        words.windows(2).find_map(|pair| {
            if pair[1].eq_ignore_ascii_case("ETH") {
                pair[0].parse::<f64>().ok()
            } else {
                None
            }
        })
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
    tracing::info!(
        "[goals] LLM parsed capital_eth={} from raw_goal={:?} (user_stated_eth={:?})",
        goal.capital_eth, body.raw_goal, user_stated_eth
    );

    if !(goal.capital_eth > 0.0) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "status": "error",
                "error": "capital_eth must be greater than zero"
            })),
        );
    }

    //ves-fix: clamp capital_eth against the wallet cap so an LLM that
    //hallucinates a large default (e.g. 1.0 ETH for "compound my ETH")
    //can't trip the keymaster CapExceeded check downstream. uses 90% of
    //the cap to leave headroom for fees and slippage.
    let original_capital_eth = goal.capital_eth;
    if let Some(cap) = resolved.cap_eth {
        let ceiling = cap * 0.9;
        if goal.capital_eth > ceiling {
            tracing::info!(
                "[goals] clamping capital_eth from {} to {} (wallet {} cap {})",
                goal.capital_eth, ceiling, body.wallet_label, cap
            );
            goal.capital_eth = ceiling;
            goal.entry_eth = ceiling;
            goal.current_eth = ceiling;
        }
    }

    //── constraint: chain must be in the registry ─────────────────
    let chain_config = match state.chain_registry.get(&goal.chain) {
        Some(c) => c,
        None => {
            let configured: Vec<String> = state
                .chain_registry
                .available()
                .iter()
                .map(|c| c.name.clone())
                .collect();
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "status": "error",
                    "error": format!(
                        "chain '{}' is not supported. Configured chains: {}",
                        goal.chain,
                        configured.join(", ")
                    )
                })),
            );
        }
    };

    let rpc_url = chain_config.rpc_url.clone();
    if rpc_url.is_empty() {
        tracing::warn!(
            "balance check skipped for wallet {} — no rpc_url for chain '{}'",
            body.wallet_label,
            goal.chain
        );
    } else {
        match fetch_wallet_balance_eth(&state.http_client, &rpc_url, &resolved.address).await {
            Ok(balance_eth) => {
                if goal.capital_eth > (balance_eth - GAS_RESERVE_ETH) {
                    tracing::warn!(
                        "goal rejected — insufficient balance for wallet {}: requested={} available={}",
                        body.wallet_label,
                        goal.capital_eth,
                        balance_eth
                    );
                    let stated = user_stated_eth.unwrap_or(original_capital_eth);
                    let detail = if stated > goal.capital_eth {
                        format!(
                            "insufficient wallet balance — you requested {:.4} ETH (clamped to {:.4} ETH by wallet cap), but wallet holds {:.4} ETH (reserve: {:.3} ETH)",
                            stated, goal.capital_eth, balance_eth, GAS_RESERVE_ETH
                        )
                    } else {
                        format!(
                            "insufficient wallet balance — requested {:.4} ETH but wallet holds {:.4} ETH (reserve: {:.3} ETH)",
                            goal.capital_eth, balance_eth, GAS_RESERVE_ETH
                        )
                    };
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({
                            "status": "error",
                            "error": detail
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

    //set status to running and spawn the goalrunner
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
    let runners_for_cleanup = state.goal_runners.clone();
    let txs_for_cleanup = state.goal_cancel_txs.clone();
    let handle = tokio::spawn(async move {
        crate::goal_runner::run_goal(goal_id, cancel_rx, deps).await;
        //ves-mem: drop runner + cancel sender from shared maps so they
        //don't accumulate across the gateway's lifetime.
        runners_for_cleanup.lock().await.remove(&goal_id);
        txs_for_cleanup.lock().await.remove(&goal_id);
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

    //ves-103: never silently 201 with an empty body — the client needs the
    //goal_id. surface a 500 if serialization fails so they can retry.
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
    //send cancel signal to runner
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
        //simulate: two running goals, check if wallet "safe" has an active goal
        let goals = vec![
            make_goal("safe", GoalStatus::Running, GoalStrategy::Adaptive, 0.1, 0.1),
            make_goal("hot", GoalStatus::Completed, GoalStrategy::Snipe, 0.05, 0.06),
        ];

        let running: Vec<_> = goals.iter().filter(|g| g.status == GoalStatus::Running).collect();

        //"safe" wallet has a running goal — should be rejected
        let conflict = running.iter().find(|g| g.wallet_label == "safe");
        assert!(conflict.is_some(), "should find active goal for wallet 'safe'");

        //"hot" wallet has no running goal — should be allowed
        let no_conflict = running.iter().find(|g| g.wallet_label == "hot");
        assert!(no_conflict.is_none(), "wallet 'hot' has no running goal");

        //"new" wallet has no goals at all — should be allowed
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

        //at max — should reject
        assert!(running_count >= max, "should be at max capacity");

        //below max — should allow
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

        //capital = 1.0 + 2.0 = 3.0, current = 1.1 + 1.8 = 2.9
        //pnl = -0.1, pnl_pct = -0.1/3.0 * 100 = -3.33%
        assert!((total_capital - 3.0).abs() < 1e-10);
        assert!((total_current - 2.9).abs() < 1e-10);
        assert!((total_pnl - (-0.1)).abs() < 1e-10);
        assert!((total_pnl_pct - (-100.0 / 30.0)).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_redis_save_get_list() {
        //only runs when redis is available
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

        //save
        save_goal(&client, &goal).await.expect("save_goal");

        //get
        let fetched = get_goal(&client, goal.id).await.expect("get_goal");
        assert_eq!(fetched.id, goal.id);
        assert_eq!(fetched.capital_eth, 0.1);

        //list
        let all = list_goals(&client).await.expect("list_goals");
        assert!(all.iter().any(|g| g.id == goal.id));

        //update status
        let updated = update_goal_status(&client, goal.id, GoalStatus::Cancelled)
            .await
            .expect("update_goal_status");
        assert_eq!(updated.status, GoalStatus::Cancelled);

        //cleanup
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

    #[test]
    fn test_is_terminal_classification() {
        assert!(is_terminal(&GoalStatus::Completed));
        assert!(is_terminal(&GoalStatus::Failed));
        assert!(is_terminal(&GoalStatus::Cancelled));
        assert!(!is_terminal(&GoalStatus::Pending));
        assert!(!is_terminal(&GoalStatus::Running));
        assert!(!is_terminal(&GoalStatus::Paused));
    }

    #[tokio::test]
    async fn test_save_goal_applies_ttl_for_terminal_status() {
        let client = match redis::Client::open("redis://127.0.0.1:6379") {
            Ok(c) => c,
            Err(_) => return,
        };
        if client.get_multiplexed_async_connection().await.is_err() {
            return;
        }

        let now = Utc::now();
        let mut goal = GoalSpec {
            id: Uuid::new_v4(),
            raw_goal: "ttl test".into(),
            wallet_label: "ttl-test".into(),
            wallet_id: None,
            chain: "base_sepolia".into(),
            capital_eth: 0.01,
            target_gain_pct: 10.0,
            stop_loss_pct: 5.0,
            strategy: GoalStrategy::Adaptive,
            status: GoalStatus::Running,
            cycles: 0,
            current_step: "SCOUTING".into(),
            entry_eth: 0.01,
            current_eth: 0.01,
            pnl_eth: 0.0,
            pnl_pct: 0.0,
            token_address: None,
            token_amount_held: None,
            resolved_wallet_uuid: None,
            created_at: now,
            updated_at: now,
            error: None,
        };

        save_goal(&client, &goal).await.unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let ttl: i64 = redis::cmd("TTL")
            .arg(goal_key(&goal.id))
            .query_async(&mut conn)
            .await
            .unwrap();
        //non-terminal save must NOT set a TTL.
        assert_eq!(ttl, -1, "running goal should have no TTL");

        goal.status = GoalStatus::Completed;
        save_goal(&client, &goal).await.unwrap();
        let ttl: i64 = redis::cmd("TTL")
            .arg(goal_key(&goal.id))
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(ttl > 0 && ttl <= TERMINAL_GOAL_TTL_SECS,
            "terminal goal should have a positive TTL within the 30-day window, got {ttl}");

        //cleanup
        let _: () = redis::cmd("DEL")
            .arg(goal_key(&goal.id)).query_async(&mut conn).await.unwrap();
        let _: () = redis::cmd("SREM")
            .arg(GOALS_INDEX_KEY).arg(goal.id.to_string())
            .query_async(&mut conn).await.unwrap();
    }

    #[tokio::test]
    async fn test_sweep_purges_and_backfills() {
        let client = match redis::Client::open("redis://127.0.0.1:6379") {
            Ok(c) => c,
            Err(_) => return,
        };
        if client.get_multiplexed_async_connection().await.is_err() {
            return;
        }

        let now = Utc::now();
        let make = |status: GoalStatus, age_days: i64| {
            let ts = now - chrono::Duration::days(age_days);
            GoalSpec {
                id: Uuid::new_v4(),
                raw_goal: "sweep test".into(),
                wallet_label: "sweep-test".into(),
                wallet_id: None,
                chain: "base_sepolia".into(),
                capital_eth: 0.01, target_gain_pct: 10.0, stop_loss_pct: 5.0,
                strategy: GoalStrategy::Adaptive,
                status,
                cycles: 0,
                current_step: "SCOUTING".into(),
                entry_eth: 0.01, current_eth: 0.01, pnl_eth: 0.0, pnl_pct: 0.0,
                token_address: None, token_amount_held: None, resolved_wallet_uuid: None,
                created_at: ts,
                updated_at: ts,
                error: None,
            }
        };

        //recent terminal → backfilled (within 30d)
        let recent = make(GoalStatus::Completed, 5);
        //old terminal → purged (>30d)
        let old = make(GoalStatus::Failed, 45);
        //active → left alone
        let active = make(GoalStatus::Running, 5);

        //write raw so we can bypass save_goal's TTL logic and simulate a
        //pre-migration record with no TTL.
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        for g in [&recent, &old, &active] {
            let json = serde_json::to_string(g).unwrap();
            let _: () = conn.set(goal_key(&g.id), &json).await.unwrap();
            let _: () = conn.sadd(GOALS_INDEX_KEY, g.id.to_string()).await.unwrap();
        }

        //also add an orphan index entry (pointing at a non-existent key)
        let orphan_id = Uuid::new_v4();
        let _: () = conn.sadd(GOALS_INDEX_KEY, orphan_id.to_string()).await.unwrap();

        let report = sweep_terminal_goals(&client).await.unwrap();
        assert!(report.scanned >= 4);
        assert!(report.orphan_index_entries_removed >= 1);
        assert!(report.expired_terminal_purged >= 1);
        assert!(report.terminal_ttl_backfilled >= 1);

        //old terminal goal: key gone, index entry gone
        let exists_old: i64 = redis::cmd("EXISTS").arg(goal_key(&old.id))
            .query_async(&mut conn).await.unwrap();
        assert_eq!(exists_old, 0, "old terminal goal should be purged");
        let in_index_old: bool = redis::cmd("SISMEMBER").arg(GOALS_INDEX_KEY).arg(old.id.to_string())
            .query_async(&mut conn).await.unwrap();
        assert!(!in_index_old);

        //recent terminal goal: key still present, TTL now set
        let ttl_recent: i64 = redis::cmd("TTL").arg(goal_key(&recent.id))
            .query_async(&mut conn).await.unwrap();
        assert!(ttl_recent > 0);

        //active goal: untouched, no TTL
        let ttl_active: i64 = redis::cmd("TTL").arg(goal_key(&active.id))
            .query_async(&mut conn).await.unwrap();
        assert_eq!(ttl_active, -1);

        //orphan index entry gone
        let in_index_orphan: bool = redis::cmd("SISMEMBER").arg(GOALS_INDEX_KEY).arg(orphan_id.to_string())
            .query_async(&mut conn).await.unwrap();
        assert!(!in_index_orphan);

        //cleanup surviving records
        for g in [&recent, &active] {
            let _: () = redis::cmd("DEL").arg(goal_key(&g.id)).query_async(&mut conn).await.unwrap();
            let _: () = redis::cmd("SREM").arg(GOALS_INDEX_KEY).arg(g.id.to_string())
                .query_async(&mut conn).await.unwrap();
        }
    }
}
