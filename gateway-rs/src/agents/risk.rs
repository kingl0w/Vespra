use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::data::protocol::ProtocolData;
use crate::types::decisions::{RiskDecision, RiskScore};
use crate::types::opportunity::Opportunity;

#[derive(Debug, Clone, Serialize)]
pub struct RiskContext {
    pub opportunity: Opportunity,
    pub protocol_data: ProtocolData,
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
gate_pass = true ONLY when score is LOW or MEDIUM AND honeypot_risk is not HIGH.\n\
Be conservative. When in doubt, rate higher risk.";

pub struct RiskAgent {
    llm: Arc<dyn AgentClient>,
}

impl RiskAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn assess(&self, ctx: &RiskContext) -> Result<RiskDecision> {
        let protocol_json = serde_json::to_string(&ctx.protocol_data)?;
        let task = format!(
            "LIVE_PROTOCOL_DATA: {protocol_json}\n\n\
             [TASK] Analyze risk for protocol {} pool {} on {}",
            ctx.opportunity.protocol, ctx.opportunity.pool, ctx.opportunity.chain
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let parsed: RiskRaw = serde_json::from_str(&raw).unwrap_or(RiskRaw {
            score: None,
            gate_pass: None,
            reason: Some(format!("parse_error: {raw}")),
            recommendation: None,
        });

        let score = parse_risk_score(parsed.score.as_deref().unwrap_or("HIGH"));
        let gate_pass = parse_gate_pass(&parsed.gate_pass);

        if gate_pass {
            Ok(RiskDecision::GatePass { score })
        } else {
            let reason = parsed
                .reason
                .or(parsed.recommendation)
                .unwrap_or_else(|| "gate_pass=false".into());
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
