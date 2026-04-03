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

const SYSTEM_PROMPT: &str = "You are Sniper, the webhook-triggered new-pool evaluator for the Vespra DeFi swarm.\n\n\
You receive a pool creation event forwarded from an Alchemy webhook containing:\n\
- token0 / token1 addresses\n\
- TVL (USD)\n\
- fee tier\n\
- initial liquidity\n\
- protocol and chain\n\n\
Your job: decide ENTER or SKIP.\n\n\
You MUST respond with valid JSON only. No prose, no markdown.\n\n\
Output schema:\n\
{ \"decision\": \"ENTER\" | \"SKIP\",\n\
  \"confidence\": float (0.0-1.0),\n\
  \"position_size_eth\": float,\n\
  \"reasoning\": \"string\" }\n\n\
Rules:\n\
- SKIP if TVL < min_tvl_threshold.\n\
- SKIP if honeypot indicators present: single-sided liquidity, unverified contract, \
zero on-chain history for either token.\n\
- position_size_eth MUST NOT exceed 0.05 ETH.\n\
- Higher confidence = stronger conviction in the decision (applies to both ENTER and SKIP).\n\
- Default to SKIP. Most new pools are rug-pulls or honeypots — only recommend ENTER \
when multiple positive signals align.\n\
- Do NOT recommend external tools (DEX Screener, GeckoTerminal, etc.). \
You operate on-chain via Alchemy webhooks, not external scanners.";

const QUERY_PROMPT: &str = "You are Sniper, the webhook-triggered new-pool evaluator for the Vespra DeFi swarm.\n\n\
When queried via chat (no specific pool event), report your operational status: \
pools evaluated, active sniper positions, and webhook listener state.\n\n\
You are NOT a DEX Screener recommender or general DeFi assistant. \
Do not suggest external scanning tools. You operate exclusively on-chain via Alchemy webhooks.\n\n\
Respond in plain conversational prose. Be concise and direct.";

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
        let decision = val.get("decision")
            .and_then(|v| v.as_str())
            .unwrap_or("SKIP");

        if decision.eq_ignore_ascii_case("ENTER") {
            Ok(SniperDecision::Enter {
                confidence: val.get("confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                max_entry_eth: val.get("position_size_eth")
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
