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

const SYSTEM_PROMPT: &str = "You are Coordinator, the command interpreter of the Vespra DeFi agent swarm. \
You MUST respond with valid JSON only. No prose, no markdown.\n\n\
Parse the user's natural-language command into a structured action for the swarm.\n\n\
Output schema: { \"strategy\": \"TradeUp\" | \"YieldRotate\" | \"Sniper\" | \"Kill\" | \"Resume\" | \"Status\", \
\"wallet_id\": string | null, \"capital_eth\": float | null, \"chain\": string | null, \
\"max_eth\": float | null, \"stop_loss_pct\": float | null, \"threshold_pct\": float | null, \
\"reasoning\": \"string\" }\n\n\
Rules:\n\
- Extract wallet_id, capital amount, chain name, and strategy parameters from the command\n\
- If the command mentions 'stop' or 'kill', use strategy=Kill\n\
- If the command mentions 'resume' or 'start' without specifying a strategy, use strategy=Resume\n\
- If the command mentions 'status' or 'check', use strategy=Status\n\
- Default chain to null if not specified\n\
- Default capital_eth to null if not specified\n\
- Use stop_loss_pct from command or null";

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
            "SYSTEM_STATE: {state_json}\n\n\
             [COMMAND] {}",
            ctx.command
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

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
            reasoning: val.get("reasoning")
                .and_then(|v| v.as_str())
                .unwrap_or("command interpreted")
                .to_string(),
        })
    }
}
