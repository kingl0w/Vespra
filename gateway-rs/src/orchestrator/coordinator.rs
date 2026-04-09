use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::agents::AgentClient;
use crate::config::GatewayConfig;
use crate::data::yield_provider::ProviderRegistry;

//── session context ──────────────────────────────────────────────

const SESSION_KEY: &str = "vespra:coordinator:session";
const SESSION_TTL: u64 = 3600; // 1 hour
const MAX_ENTRIES: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResult {
    pub agent: String,
    pub output: serde_json::Value,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub entries: VecDeque<AgentResult>,
    pub started_at: i64,
}

impl SessionContext {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            started_at: chrono::Utc::now().timestamp(),
        }
    }

    pub fn push(&mut self, result: AgentResult) {
        self.entries.push_back(result);
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }
}

///load session context from redis. returns empty context if not found.
pub async fn load_session(redis: &redis::Client) -> SessionContext {
    if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(redis).await {
        let raw: Option<String> = conn.get(SESSION_KEY).await.ok().flatten();
        if let Some(data) = raw {
            if let Ok(ctx) = serde_json::from_str::<SessionContext>(&data) {
                return ctx;
            }
        }
    }
    SessionContext::new()
}

///save session context to redis with ttl.
pub async fn save_session(redis: &redis::Client, ctx: &SessionContext) {
    if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(redis).await {
        if let Ok(json) = serde_json::to_string(ctx) {
            let _: Result<(), _> = conn.set_ex(SESSION_KEY, &json, SESSION_TTL).await;
        }
    }
}

///append an agent result to the session and save it.
pub async fn append_to_session(redis: &redis::Client, result: AgentResult) {
    let mut ctx = load_session(redis).await;
    ctx.push(result);
    save_session(redis, &ctx).await;
}

//── orchestration output ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationResult {
    pub summary: String,
    pub next_action: String,
    pub confidence: f64,
    pub spawn_dag: Option<String>,
    pub actions: Vec<OrchestrationAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrationAction {
    pub agent: String,
    pub instruction: String,
}

///expected llm output for validation.
#[derive(Deserialize)]
struct LlmOrchestrationOutput {
    summary: Option<String>,
    next_action: Option<String>,
    confidence: Option<f64>,
    spawn_dag: Option<String>,
    actions: Option<Vec<LlmAction>>,
}

#[derive(Deserialize)]
struct LlmAction {
    agent: Option<String>,
    instruction: Option<String>,
}

//── orchestration system prompt ──────────────────────────────────

const ORCHESTRATE_PROMPT: &str = "You are a DeFi portfolio orchestrator. \
You receive structured data from specialized agents and synthesize it into clear action plans.\n\n\
You MUST respond with valid JSON only. Never add explanation outside the JSON.\n\n\
Output schema: { \"summary\": \"string\", \"next_action\": \"string\", \
\"confidence\": float (0.0-1.0), \"spawn_dag\": \"yield_rotation\" | \"trade_up\" | \"rebalance\" | null, \
\"actions\": [ { \"agent\": \"string\", \"instruction\": \"string\" } ] }\n\n\
Constraints:\n\
- Do not invent data. If context is insufficient, set confidence below 0.5 and recommend gathering more data.\n\
- spawn_dag: Only set this if you are confident a multi-step pipeline is needed.\n\
  Valid values: \"yield_rotation\", \"trade_up\", \"rebalance\", null.\n\
- actions: List of specific instructions for agents to execute. Each action should name the agent and what it should do.\n\
- summary: A 1-3 sentence overview of the current situation.\n\
- next_action: The single most important next step.\n\
- confidence: 0.0-1.0 score reflecting how much data you have to make a recommendation.";

//── coordinator orchestrator ─────────────────────────────────────

pub struct CoordinatorOrchestrator {
    llm: Arc<dyn AgentClient>,
    redis: Arc<redis::Client>,
    config: Arc<GatewayConfig>,
    yield_registry: Arc<ProviderRegistry>,
}

impl CoordinatorOrchestrator {
    pub fn new(
        llm: Arc<dyn AgentClient>,
        redis: Arc<redis::Client>,
        config: Arc<GatewayConfig>,
        yield_registry: Arc<ProviderRegistry>,
    ) -> Self {
        Self {
            llm,
            redis,
            config,
            yield_registry,
        }
    }

    ///main orchestration entry point.
    pub async fn orchestrate(
        &self,
        intent: &str,
        wallet: Option<&str>,
        chain: Option<&str>,
    ) -> Result<OrchestrationResult> {
        //1. load session context
        let mut session = load_session(&self.redis).await;

        //2. if context is sparse, populate with scout + yield data
        if session.entries.len() < 3 {
            self.populate_context(&mut session, chain).await;
        }

        //3. build llm prompt
        let context_summary = session
            .entries
            .iter()
            .map(|e| {
                let ts = chrono::DateTime::from_timestamp(e.timestamp, 0)
                    .map(|dt| dt.format("%H:%M:%S").to_string())
                    .unwrap_or_else(|| "??:??:??".into());
                let output_str = serde_json::to_string(&e.output)
                    .unwrap_or_else(|_| "{}".into());
                //truncate large outputs
                let truncated = if output_str.len() > 500 {
                    format!("{}...", &output_str[..500])
                } else {
                    output_str
                };
                format!("[{ts}] {}: {truncated}", e.agent)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let task = format!(
            "SESSION CONTEXT ({} entries):\n{}\n\n\
             INTENT: {intent}\n\
             WALLET: {}\n\
             CHAIN: {}\n\n\
             [TASK] Synthesize the session context and intent into an orchestration plan.",
            session.entries.len(),
            if context_summary.is_empty() {
                "(empty)".to_string()
            } else {
                context_summary
            },
            wallet.unwrap_or("not specified"),
            chain.unwrap_or("not specified"),
        );

        let raw = self.llm.call(ORCHESTRATE_PROMPT, &task).await?;

        //4. parse and validate output
        let result = match serde_json::from_str::<LlmOrchestrationOutput>(&raw) {
            Ok(output) => {
                //validate spawn_dag value
                let spawn_dag = output.spawn_dag.and_then(|d| {
                    let valid = ["yield_rotation", "trade_up", "rebalance"];
                    if valid.contains(&d.as_str()) {
                        Some(d)
                    } else {
                        tracing::warn!(
                            "coordinator: invalid spawn_dag value '{d}', ignoring"
                        );
                        None
                    }
                });

                OrchestrationResult {
                    summary: output
                        .summary
                        .unwrap_or_else(|| "No summary provided".into()),
                    next_action: output
                        .next_action
                        .unwrap_or_else(|| "gather_data".into()),
                    confidence: output.confidence.unwrap_or(0.0).clamp(0.0, 1.0),
                    spawn_dag,
                    actions: output
                        .actions
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|a| {
                            Some(OrchestrationAction {
                                agent: a.agent?,
                                instruction: a.instruction.unwrap_or_default(),
                            })
                        })
                        .collect(),
                }
            }
            Err(parse_err) => {
                //try generic value parse
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(val) => OrchestrationResult {
                        summary: val
                            .get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or("LLM output partially parsed")
                            .to_string(),
                        next_action: val
                            .get("next_action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("review_output")
                            .to_string(),
                        confidence: val
                            .get("confidence")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.3)
                            .clamp(0.0, 1.0),
                        spawn_dag: None,
                        actions: vec![],
                    },
                    Err(_) => {
                        tracing::warn!(
                            "coordinator: LLM returned invalid JSON: {parse_err}. Raw: {}",
                            &raw[..raw.len().min(500)]
                        );
                        return Err(anyhow::anyhow!(
                            "Coordinator LLM output is not valid JSON: {parse_err}"
                        ));
                    }
                }
            }
        };

        //5. append this orchestration result to session
        session.push(AgentResult {
            agent: "coordinator".into(),
            output: serde_json::to_value(&result).unwrap_or_default(),
            timestamp: chrono::Utc::now().timestamp(),
        });
        save_session(&self.redis, &session).await;

        Ok(result)
    }

    ///populate session context with quick scout + yield data.
    async fn populate_context(&self, session: &mut SessionContext, chain: Option<&str>) {
        let now = chrono::Utc::now().timestamp();
        let chain_filter = chain.or(self.config.chains.first().map(|s| s.as_str()));

        //fetch yield pool data
        match self
            .yield_registry
            .fetch_pools(
                chain_filter,
                self.config.yield_min_tvl_usd,
                self.config.yield_min_apy,
            )
            .await
        {
            Ok(pools) => {
                let top: Vec<_> = pools.into_iter().take(5).collect();
                session.push(AgentResult {
                    agent: "scout_yield".into(),
                    output: serde_json::json!({
                        "type": "yield_pools",
                        "count": top.len(),
                        "pools": top,
                    }),
                    timestamp: now,
                });
            }
            Err(e) => {
                tracing::warn!("coordinator: failed to fetch yield data for context: {e}");
            }
        }

        save_session(&self.redis, session).await;
    }

    ///spawn a dag on nullboiler (fire-and-forget).
    pub async fn spawn_dag(
        &self,
        dag_name: &str,
        wallet: Option<&str>,
        chain: Option<&str>,
    ) {
        let url = format!(
            "{}/runs",
            self.config.nullboiler_url.trim_end_matches('/')
        );

        let body = serde_json::json!({
            "dag": dag_name,
            "wallet": wallet,
            "chain": chain,
        });

        let client = reqwest::Client::new();
        match client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                tracing::info!(
                    "coordinator: spawned dag '{}' on nullboiler → {} {}",
                    dag_name,
                    status,
                    &text[..text.len().min(200)]
                );
            }
            Err(e) => {
                tracing::warn!(
                    "coordinator: failed to spawn dag '{}' on nullboiler: {e}",
                    dag_name
                );
            }
        }
    }
}
