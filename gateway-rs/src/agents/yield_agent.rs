use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::types::decisions::YieldDecision;

#[derive(Debug, Clone, Serialize)]
pub struct YieldContext {
    pub current_position: Option<CurrentPosition>,
    pub candidates: Vec<YieldCandidate>,
    pub threshold_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentPosition {
    pub protocol: String,
    pub apy_pct: f64,
    pub amount_eth: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct YieldCandidate {
    pub protocol: String,
    pub pool_id: String,
    pub apy_pct: f64,
    pub chain: String,
    pub tvl_usd: u64,
    pub momentum_score: f64,
}

const SYSTEM_PROMPT: &str = "You are Yield, the yield rotation specialist of the Vespra DeFi agent swarm. \
You MUST respond with valid JSON only. No prose, no markdown.\n\n\
Compare the current yield position against candidate pools. Recommend rebalancing \
only when a candidate offers a materially higher APY above the threshold.\n\n\
Output schema: { \"action\": \"Rebalance\" | \"Hold\", \"target_protocol\": \"string\", \
\"target_pool_id\": \"string\", \"expected_apy_gain_pct\": float, \"reasoning\": \"string\" }\n\n\
Rules:\n\
- Only recommend Rebalance if expected_apy_gain_pct >= threshold_pct\n\
- If no current position, recommend the best candidate as Rebalance\n\
- Consider TVL, protocol reputation, and momentum when deciding\n\
- Be conservative: gas costs and slippage erode small gains";

pub struct YieldAgent {
    llm: Arc<dyn AgentClient>,
}

impl YieldAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn evaluate(&self, ctx: &YieldContext) -> Result<YieldDecision> {
        let ctx_json = serde_json::to_string(ctx)?;
        let task = format!(
            "YIELD_CONTEXT: {ctx_json}\n\n\
             [TASK] Evaluate whether to rotate yield position. Threshold = {:.2}%",
            ctx.threshold_pct
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let action = val.get("action").and_then(|v| v.as_str()).unwrap_or("Hold");

        if action.eq_ignore_ascii_case("Rebalance") {
            Ok(YieldDecision::Rebalance {
                target_protocol: val.get("target_protocol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                target_pool_id: val.get("target_pool_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                expected_gain_pct: val.get("expected_apy_gain_pct")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                reasoning: val.get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("rebalance recommended")
                    .to_string(),
            })
        } else {
            Ok(YieldDecision::Hold {
                reasoning: val.get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hold — no better opportunity")
                    .to_string(),
            })
        }
    }
}
