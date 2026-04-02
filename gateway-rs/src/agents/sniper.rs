use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::types::decisions::SniperDecision;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperContext {
    pub pool_address: String,
    pub token0: String,
    pub token1: String,
    pub tvl_usd: f64,
    pub protocol: String,
    pub chain: String,
    pub min_tvl_threshold: f64,
}

const SYSTEM_PROMPT: &str = "You are the Sniper agent for the Vespra DeFi swarm. \
Your role is to evaluate new liquidity pools detected via on-chain webhooks (Alchemy) and \
decide whether to enter a position.\n\n\
You receive pool data (address, TVL, creation block, token pair) and return a structured \
entry/skip decision. You MUST respond with valid JSON only. No prose, no markdown.\n\n\
Output schema: { \"entry_recommended\": bool, \"confidence\": float (0-1), \
\"max_entry_eth\": float, \"reasoning\": \"string\" }\n\n\
Rules:\n\
- Only recommend entry if TVL >= min_tvl_threshold\n\
- Check for honeypot indicators: single-sided liquidity, unknown tokens, no verified contract\n\
- Higher confidence = stronger recommendation\n\
- max_entry_eth should never exceed 0.05 ETH for safety\n\
- Be extremely cautious — most new pools are high risk";

const QUERY_PROMPT: &str = "You are the Sniper agent for the Vespra DeFi swarm. \
Your role is to evaluate new liquidity pools detected via on-chain Alchemy webhooks and \
decide whether to enter a position.\n\n\
When queried via chat without a specific pool to evaluate, report your current status: \
how many pools you have evaluated, any active sniper positions, and whether the webhook \
listener is active. Do not give general advice about external tools like DEX Screener or \
GeckoTerminal — you operate on-chain via webhooks, not via external scanners.\n\n\
Respond in helpful prose. Be concise and direct.";

pub struct SniperAgent {
    llm: Arc<dyn AgentClient>,
}

impl SniperAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn evaluate(&self, ctx: &SniperContext) -> Result<SniperDecision> {
        let ctx_json = serde_json::to_string(ctx)?;
        let task = format!(
            "POOL_EVENT: {ctx_json}\n\n\
             [TASK] Evaluate new pool for early entry. Min TVL threshold: ${:.0}",
            ctx.min_tvl_threshold
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let recommended = val.get("entry_recommended")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if recommended {
            Ok(SniperDecision::Enter {
                confidence: val.get("confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                max_entry_eth: val.get("max_entry_eth")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.01),
                reasoning: val.get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("entry recommended")
                    .to_string(),
            })
        } else {
            Ok(SniperDecision::Pass {
                reasoning: val.get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pass — too risky")
                    .to_string(),
            })
        }
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        self.llm.call(QUERY_PROMPT, question).await
    }
}
