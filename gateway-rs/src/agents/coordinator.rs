use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::types::decisions::CommandIntent;

#[derive(Debug, Clone, Serialize)]
pub struct CoordinatorContext {
    pub command: String,
    pub system_state: SystemState,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemState {
    pub active_loops: Vec<String>,
    pub kill_flag: bool,
    pub wallet_count: usize,
    pub chains: Vec<String>,
}

const BASE_PROMPT: &str = "You are Coordinator, the command interpreter and orchestrator of the Vespra DeFi agent swarm.\n\n\
Your behaviour depends on the MODE field in the prompt context.\n\n\
── MODE: chat (/swarm/command) ──────────────────────────────\n\
Respond in plain conversational prose ONLY. No JSON. No markdown code fences.\n\
Parse the user's natural-language command, execute the appropriate strategy internally, \
and report the result conversationally.\n\n\
── MODE: orchestrate (/coordinator/orchestrate) ─────────────\n\
Respond with valid JSON ONLY matching the OrchestrationResult schema. \
No prose preamble, no markdown.\n\n\
Output schema (orchestrate mode): { \"strategy\": \"TradeUp\" | \"YieldRotate\" | \"Sniper\" | \"Kill\" | \"Resume\" | \"Status\" \
| \"AskCoordinator\" | \"AskScout\" | \"AskRisk\" | \"AskSentinel\" | \"AskTrader\" | \"AskYield\" | \"AskSniper\" | \"AskLauncher\" | \"AskExecutor\", \
\"wallet_id\": string | null, \"capital_eth\": float | null, \"chain\": string | null, \
\"max_eth\": float | null, \"stop_loss_pct\": float | null, \"threshold_pct\": float | null, \
\"query\": string | null, \
\"reasoning\": \"string\" }\n\n\
Strategy routing rules (both modes):\n\
- Extract wallet_id, capital amount, chain name, and strategy parameters from the command\n\
- If the command mentions 'stop' or 'kill', use strategy=Kill\n\
- If the command mentions 'resume' without specifying a strategy, use strategy=Resume\n\
- If the command mentions 'status' or 'check' about the system/swarm, use strategy=Status\n\
- Use AskScout when the user wants yield opportunities, pool data, APY info, or market scanning\n\
- Use AskRisk when the user wants a risk assessment of a protocol, token, or position\n\
- Use AskSentinel when the user wants position health, wallet monitoring, or alert status\n\
- Use AskTrader when the user wants swap quotes, trade routes, or price impact analysis\n\
- Use AskYield when the user wants current lending positions, deposit/withdraw recommendations\n\
- Use AskSniper when the user wants new pool detection or sniper position info\n\
- Use AskLauncher when the user wants token deployment info or launch planning\n\
- Use AskExecutor when the user wants wallet balances, transaction history, or execution status\n\
- For AskX strategies, set \"query\" to the user's original question verbatim. Set query to null for non-Ask strategies.\n\
- Use TradeUp only when user explicitly wants to START a trade-up loop\n\
- Use YieldRotate only when user explicitly wants to ROTATE yield positions\n\
- Use Sniper only when user explicitly wants to ENABLE sniper auto-entry\n\
- Default chain to null if not specified\n\
- Default capital_eth to null if not specified\n\
- Use stop_loss_pct from command or null\n\
- If the command has an [agent_name] prefix like [scout] or [risk], use the matching AskX strategy\n\
- Use AskCoordinator for general portfolio summaries, activity reports, or when the user chats with the coordinator directly\n\
- For AskCoordinator, set query to the user's message — the coordinator will respond in plain prose";

pub struct CoordinatorAgent {
    llm: Arc<dyn AgentClient>,
}

impl CoordinatorAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn interpret(&self, ctx: &CoordinatorContext) -> Result<CommandIntent> {
        let state_json = serde_json::to_string(&ctx.system_state)?;
        let task = format!(
            "MODE: orchestrate\n\n\
             SYSTEM_STATE: {state_json}\n\n\
             [COMMAND] {}",
            ctx.command
        );

        let raw = self.llm.call(BASE_PROMPT, &task).await?;

        let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();

        Ok(CommandIntent {
            strategy: val.get("strategy")
                .and_then(|v| v.as_str())
                .unwrap_or("Status")
                .to_string(),
            wallet_id: val.get("wallet_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            capital_eth: val.get("capital_eth")
                .and_then(|v| v.as_f64()),
            chain: val.get("chain")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            max_eth: val.get("max_eth")
                .and_then(|v| v.as_f64()),
            stop_loss_pct: val.get("stop_loss_pct")
                .and_then(|v| v.as_f64()),
            threshold_pct: val.get("threshold_pct")
                .and_then(|v| v.as_f64()),
            query: val.get("query")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            reasoning: val.get("reasoning")
                .and_then(|v| v.as_str())
                .unwrap_or("command interpreted")
                .to_string(),
        })
    }

    /// Respond to a general chat query in plain prose.
    pub async fn query(&self, question: &str) -> Result<String> {
        let task = format!("MODE: chat\n\n[QUERY] {question}");
        self.llm.call(BASE_PROMPT, &task).await
    }
}
