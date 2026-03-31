use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::types::decisions::LaunchDecision;

#[derive(Debug, Clone, Serialize)]
pub struct LauncherContext {
    pub name: String,
    pub symbol: String,
    pub supply: u64,
    pub decimals: u8,
    pub chain: String,
    pub liquidity_eth: f64,
}

const SYSTEM_PROMPT: &str = "You are Launcher, the token deployment specialist of the Vespra DeFi agent swarm. \
You MUST respond with valid JSON only. No prose, no markdown.\n\n\
Evaluate whether a proposed ERC-20 token deployment is reasonable and safe.\n\n\
Output schema: { \"approved\": bool, \"suggested_liquidity_eth\": float, \"reasoning\": \"string\" }\n\n\
Rules:\n\
- Reject if supply is unreasonably high (>1 trillion) or zero\n\
- Reject if decimals > 18\n\
- Reject if liquidity_eth > 1.0 ETH (safety cap for testing)\n\
- Suggest appropriate liquidity based on supply and market conditions\n\
- Ensure name and symbol are reasonable (not empty, not offensive)\n\
- Be conservative with liquidity suggestions";

pub struct LauncherAgent {
    llm: Arc<dyn AgentClient>,
}

impl LauncherAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn evaluate(&self, ctx: &LauncherContext) -> Result<LaunchDecision> {
        let ctx_json = serde_json::to_string(ctx)?;
        let task = format!(
            "TOKEN_SPEC: {ctx_json}\n\n\
             [TASK] Evaluate token deployment proposal"
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let approved = val.get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if approved {
            Ok(LaunchDecision::Approved {
                suggested_liquidity_eth: val.get("suggested_liquidity_eth")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.01),
                reasoning: val.get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("deployment approved")
                    .to_string(),
            })
        } else {
            Ok(LaunchDecision::Rejected {
                reasoning: val.get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("deployment rejected")
                    .to_string(),
            })
        }
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        let prompt = format!("{}\n\nHowever, for this request respond with helpful prose or JSON as appropriate. \
            Do not restrict yourself to the launch schema — answer the user's question directly.", SYSTEM_PROMPT);
        self.llm.call(&prompt, question).await
    }
}
