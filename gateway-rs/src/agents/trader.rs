use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::data::quote::SwapQuote;
use crate::types::decisions::{RiskScore, TraderDecision};
use crate::types::opportunity::Opportunity;

#[derive(Debug, Clone, Serialize)]
pub struct TraderContext {
    pub opportunity: Opportunity,
    pub quote: SwapQuote,
    pub capital_eth: f64,
    pub risk_score: RiskScore,
    pub min_gain_pct: f64,
    pub max_eth: f64,
}

#[derive(Debug, Deserialize)]
struct TraderRaw {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    token_in: Option<String>,
    #[serde(default)]
    token_out: Option<String>,
    #[serde(default)]
    amount_in_wei: Option<String>,
    #[serde(default)]
    expected_gain_pct: Option<f64>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

const SYSTEM_PROMPT: &str = "You are Trader, the swap specialist of the Vespra DeFi agent swarm. \
You MUST respond with valid JSON only.\n\n\
Output strict JSON only: {\"action\": \"swap\"|\"hold\"|\"exit\", \
\"token_in\": \"<address>\", \"token_out\": \"<address>\", \
\"amount_in_wei\": \"<wei>\", \"expected_gain_pct\": <float>, \
\"reasoning\": \"<one line>\"}\n\n\
Rules:\n\
- Only swap if momentum_score >= 0.6\n\
- If risk_score is HIGH: output action=\"hold\"\n\
- If stop_loss_triggered: output action=\"exit\"";

pub struct TraderAgent {
    llm: Arc<dyn AgentClient>,
}

impl TraderAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn evaluate(&self, ctx: &TraderContext) -> Result<TraderDecision> {
        let task = serde_json::to_string(ctx)?;
        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let parsed: TraderRaw = match serde_json::from_str(&raw) {
            Ok(p) => p,
            Err(_) => {
                return Ok(TraderDecision::Hold {
                    reason: "parse_error".into(),
                });
            }
        };

        let action = parsed.action.as_deref().unwrap_or("hold").to_lowercase();
        let reasoning = parsed
            .reasoning
            .or(parsed.reason)
            .unwrap_or_else(|| "no reasoning provided".into());

        match action.as_str() {
            "swap" => Ok(TraderDecision::Swap {
                token_in: parsed.token_in.unwrap_or_default(),
                token_out: parsed.token_out.unwrap_or_default(),
                amount_in_wei: parsed.amount_in_wei.unwrap_or_default(),
                expected_gain_pct: parsed.expected_gain_pct.unwrap_or(0.0),
                reasoning,
            }),
            "exit" => Ok(TraderDecision::Exit { reason: reasoning }),
            _ => Ok(TraderDecision::Hold { reason: reasoning }),
        }
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        let prompt = format!("{}\n\nHowever, for this request respond with helpful prose or JSON as appropriate. \
            Do not restrict yourself to the swap schema — answer the user's question directly.", SYSTEM_PROMPT);
        self.llm.call(&prompt, question).await
    }
}
