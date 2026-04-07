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
    /// Chain the goal is targeting. Used to apply testnet-vs-mainnet trade rules.
    /// Defaults to the opportunity's chain if not set explicitly by the caller.
    #[serde(default)]
    pub chain: String,
}

fn is_testnet_chain(chain: &str) -> bool {
    let c = chain.to_lowercase();
    c.contains("sepolia") || c.contains("testnet") || c.contains("goerli")
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
MOMENTUM RULES (depend on chain type — the task message tells you whether the chain is TESTNET or MAINNET):\n\
• MAINNET chains: only swap if momentum_score >= 0.6.\n\
• TESTNET chains (anything containing 'sepolia', 'testnet', or 'goerli'): \
testnet pools have no real price history so momentum_score is meaningless. \
SKIP the momentum check entirely — treat the threshold as 0.0 and swap if risk allows. \
The goal here is to validate execution, not to police entry timing.\n\n\
Other rules (apply on all chains):\n\
- If risk_score is HIGH or CRITICAL: output action=\"hold\"\n\
- If stop_loss_triggered: output action=\"exit\"\n\
- When you swap, fill token_in/token_out/amount_in_wei from the provided quote.";

pub struct TraderAgent {
    llm: Arc<dyn AgentClient>,
}

impl TraderAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn evaluate(&self, ctx: &TraderContext) -> Result<TraderDecision> {
        let effective_chain = if ctx.chain.is_empty() {
            ctx.opportunity.chain.clone()
        } else {
            ctx.chain.clone()
        };
        let testnet = is_testnet_chain(&effective_chain);
        let chain_label = if testnet { "TESTNET" } else { "MAINNET" };

        let ctx_json = serde_json::to_string(ctx)?;
        let task = format!(
            "[CHAIN_TYPE] {chain_label} (chain={effective_chain})\n\n\
             [CONTEXT] {ctx_json}\n\n\
             [TASK] Decide swap/hold/exit per the {chain_label} momentum rules in the system prompt.",
        );
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
            .clone()
            .or(parsed.reason.clone())
            .unwrap_or_else(|| "no reasoning provided".into());

        // Deterministic override: on testnet, if the LLM held due to momentum
        // (despite the prompt instructing it to skip the gate), promote to a swap
        // using the quote we already fetched. Risk-based holds are still respected.
        if testnet
            && action == "hold"
            && !matches!(ctx.risk_score, RiskScore::High | RiskScore::Critical)
            && reasoning.to_lowercase().contains("momentum")
            && !ctx.quote.token_in.is_empty()
            && !ctx.quote.token_out.is_empty()
            && !ctx.quote.amount_in_wei.is_empty()
        {
            tracing::info!(
                "[trader] testnet override: HOLD(momentum) → SWAP using quote ({} → {} amount={})",
                ctx.quote.token_in,
                ctx.quote.token_out,
                ctx.quote.amount_in_wei
            );
            return Ok(TraderDecision::Swap {
                token_in: ctx.quote.token_in.clone(),
                token_out: ctx.quote.token_out.clone(),
                amount_in_wei: ctx.quote.amount_in_wei.clone(),
                expected_gain_pct: 0.0,
                reasoning: format!(
                    "[testnet override] momentum gate skipped on {effective_chain}; original LLM reason: {reasoning}"
                ),
            });
        }

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
