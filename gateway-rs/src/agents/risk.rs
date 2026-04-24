use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::config::GatewayConfig;
use crate::data::protocol::ProtocolData;
use crate::types::decisions::{RiskDecision, RiskScore};
use crate::types::opportunity::Opportunity;

#[derive(Debug, Clone, Serialize)]
pub struct RiskContext {
    pub opportunity: Opportunity,
    pub protocol_data: ProtocolData,
    ///chain the goal is targeting. passed through to the LLM prompt so it
    ///can reason about chain-specific protocols, but the gate decision is
    ///driven by NETWORK_MODE, not the chain name.
    #[serde(default)]
    pub chain: String,
}

#[derive(Debug, Deserialize)]
struct RiskRaw {
    #[serde(default)]
    score: Option<String>,
    #[serde(default)]
    gate_pass: Option<serde_json::Value>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    recommendation: Option<String>,
}

const SYSTEM_PROMPT: &str = "You are Risk, safety evaluator of the Vespra DeFi agent swarm. \
You MUST respond with valid JSON only.\n\n\
Output schema: { \"protocol\": \"string\", \"chain\": \"string\", \
\"score\": \"LOW|MEDIUM|HIGH|CRITICAL\", \"factors\": [...], \
\"gate_pass\": true|false, \"recommendation\": \"string\" }\n\n\
GATE RULES (depend on chain type — the task message tells you whether the chain is TESTNET or MAINNET):\n\
• MAINNET chains: gate_pass = true ONLY when score is LOW AND honeypot_risk is not HIGH. \
Be conservative — when in doubt, rate higher risk.\n\
• TESTNET chains (anything containing 'sepolia', 'testnet', or 'goerli'): \
the goal here is to validate execution, not to police liquidity. \
gate_pass = true for LOW or MEDIUM risk. \
gate_pass = true for HIGH risk UNLESS the pool has zero TVL or is clearly a honeypot. \
gate_pass = false only for CRITICAL risk or confirmed honeypots.\n\n\
Always score risk honestly — only the gate_pass threshold differs between testnet and mainnet.";

pub struct RiskAgent {
    llm: Arc<dyn AgentClient>,
    config: Arc<GatewayConfig>,
}

impl RiskAgent {
    pub fn new(llm: Arc<dyn AgentClient>, config: Arc<GatewayConfig>) -> Self {
        Self { llm, config }
    }

    pub async fn assess(&self, ctx: &RiskContext) -> Result<RiskDecision> {
        let protocol_json = serde_json::to_string(&ctx.protocol_data)?;

        //prefer the goal's chain (set by the runner); fall back to the opportunity's.
        let effective_chain = if ctx.chain.is_empty() {
            ctx.opportunity.chain.clone()
        } else {
            ctx.chain.clone()
        };
        let testnet = self.config.is_testnet();
        let chain_label = if testnet { "TESTNET" } else { "MAINNET" };

        let task = format!(
            "LIVE_PROTOCOL_DATA: {protocol_json}\n\n\
             [CHAIN_TYPE] {chain_label} (chain={effective_chain})\n\n\
             [TASK] Analyze risk for protocol {} pool {} on {}. \
             Apply the {chain_label} gate rules from the system prompt.",
            ctx.opportunity.protocol, ctx.opportunity.pool, effective_chain
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let parsed: RiskRaw = serde_json::from_str(&raw).unwrap_or(RiskRaw {
            score: None,
            gate_pass: None,
            reason: Some(format!("parse_error: {raw}")),
            recommendation: None,
        });

        let score = parse_risk_score(parsed.score.as_deref().unwrap_or("HIGH"));
        let llm_gate_pass = parse_gate_pass(&parsed.gate_pass);

        let gate_pass = if testnet {
            match score {
                RiskScore::Low => true,
                RiskScore::Medium => {
                    tracing::info!(
                        "[risk] testnet mode — accepting MEDIUM risk (would reject on mainnet)"
                    );
                    true
                }
                RiskScore::High => ctx.opportunity.tvl_usd > 0,
                RiskScore::Critical => false,
            }
        } else {
            //mainnet: strict low-only gate.
            matches!(score, RiskScore::Low) && llm_gate_pass
        };

        if gate_pass {
            Ok(RiskDecision::GatePass { score })
        } else {
            let reason = parsed
                .reason
                .or(parsed.recommendation)
                .unwrap_or_else(|| {
                    format!("gate_pass=false ({chain_label}, score={score:?})")
                });
            Ok(RiskDecision::GateBlock { score, reason })
        }
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        let prompt = format!("{}\n\nHowever, for this request respond with helpful prose or JSON as appropriate. \
            Do not restrict yourself to the gate_pass schema — answer the user's question directly.", SYSTEM_PROMPT);
        self.llm.call(&prompt, question).await
    }
}

fn parse_risk_score(s: &str) -> RiskScore {
    match s.to_uppercase().as_str() {
        "LOW" => RiskScore::Low,
        "MEDIUM" => RiskScore::Medium,
        "HIGH" => RiskScore::High,
        "CRITICAL" => RiskScore::Critical,
        _ => RiskScore::High,
    }
}

fn parse_gate_pass(val: &Option<serde_json::Value>) -> bool {
    match val {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
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

    fn test_ctx() -> RiskContext {
        RiskContext {
            opportunity: Opportunity {
                chain: "base".into(),
                tvl_usd: 100_000,
                ..Default::default()
            },
            protocol_data: ProtocolData::default(),
            chain: "base".into(),
        }
    }

    #[tokio::test]
    async fn medium_risk_rejected_on_mainnet() {
        let llm = Arc::new(MockLlm(
            r#"{"score":"MEDIUM","gate_pass":true,"reason":"borderline"}"#.into(),
        ));
        let risk = RiskAgent::new(llm, test_config("mainnet"));
        let decision = risk.assess(&test_ctx()).await.unwrap();
        assert!(
            decision.is_blocked(),
            "MEDIUM risk must block on mainnet, got: {decision:?}"
        );
        assert!(matches!(decision.score(), RiskScore::Medium));
    }

    #[tokio::test]
    async fn medium_risk_accepted_on_testnet() {
        let llm = Arc::new(MockLlm(
            r#"{"score":"MEDIUM","gate_pass":false,"reason":"borderline"}"#.into(),
        ));
        let risk = RiskAgent::new(llm, test_config("testnet"));
        let decision = risk.assess(&test_ctx()).await.unwrap();
        assert!(
            !decision.is_blocked(),
            "MEDIUM risk must pass on testnet, got: {decision:?}"
        );
    }
}
