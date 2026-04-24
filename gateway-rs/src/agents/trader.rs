use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::config::GatewayConfig;
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
    ///chain the goal is targeting. passed through to the LLM prompt so it
    ///can reason about chain-specific protocols, but momentum gating is
    ///driven by NETWORK_MODE, not the chain name.
    #[serde(default)]
    pub chain: String,
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
    config: Arc<GatewayConfig>,
}

impl TraderAgent {
    pub fn new(llm: Arc<dyn AgentClient>, config: Arc<GatewayConfig>) -> Self {
        Self { llm, config }
    }

    pub async fn evaluate(&self, ctx: &TraderContext) -> Result<TraderDecision> {
        let effective_chain = if ctx.chain.is_empty() {
            ctx.opportunity.chain.clone()
        } else {
            ctx.chain.clone()
        };
        let testnet = self.config.is_testnet();
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

        if testnet
            && action == "hold"
            && !matches!(ctx.risk_score, RiskScore::High | RiskScore::Critical)
            && reasoning.to_lowercase().contains("momentum")
            && !ctx.quote.token_in.is_empty()
            && !ctx.quote.token_out.is_empty()
            && !ctx.quote.amount_in_wei.is_empty()
        {
            tracing::info!("[trader] testnet mode — skipping momentum threshold");
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct MockLlm(String);

    #[async_trait]
    impl AgentClient for MockLlm {
        async fn call(&self, _system: &str, _task: &str) -> Result<String> {
            Ok(self.0.clone())
        }
    }

    fn test_config(mode: &str) -> Arc<GatewayConfig> {
        let cfg: GatewayConfig =
            serde_json::from_value(serde_json::json!({ "network_mode": mode })).unwrap();
        Arc::new(cfg)
    }

    fn test_ctx() -> TraderContext {
        TraderContext {
            opportunity: Opportunity {
                chain: "base".into(),
                ..Default::default()
            },
            quote: SwapQuote {
                token_in: "0xaaaa".into(),
                token_out: "0xbbbb".into(),
                amount_in_wei: "1000000000000000".into(),
                ..Default::default()
            },
            capital_eth: 0.01,
            risk_score: RiskScore::Low,
            min_gain_pct: 1.0,
            max_eth: 0.05,
            chain: "base".into(),
        }
    }

    #[tokio::test]
    async fn low_momentum_holds_on_mainnet() {
        let llm = Arc::new(MockLlm(
            r#"{"action":"hold","reasoning":"momentum_score 0.3 below 0.6 threshold"}"#.into(),
        ));
        let trader = TraderAgent::new(llm, test_config("mainnet"));
        let decision = trader.evaluate(&test_ctx()).await.unwrap();
        assert!(
            matches!(decision, TraderDecision::Hold { .. }),
            "low momentum on mainnet must hold, got: {decision:?}"
        );
    }

    #[tokio::test]
    async fn low_momentum_proceeds_on_testnet() {
        let llm = Arc::new(MockLlm(
            r#"{"action":"hold","reasoning":"momentum_score 0.3 below 0.6 threshold"}"#.into(),
        ));
        let trader = TraderAgent::new(llm, test_config("testnet"));
        let decision = trader.evaluate(&test_ctx()).await.unwrap();
        assert!(
            matches!(decision, TraderDecision::Swap { .. }),
            "low momentum on testnet must swap via override, got: {decision:?}"
        );
    }
}
