use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::types::decisions::ScoutDecision;
use crate::types::opportunity::{EntrySignal, Opportunity, RiskTier};

#[derive(Debug, Clone, Serialize)]
pub struct ScoutContext {
    pub wallet_id: uuid::Uuid,
    pub mode: String,
    pub pools: Vec<Opportunity>,
    pub chains: Vec<String>,
}

const SYSTEM_PROMPT: &str = "You are Scout, market intelligence agent of the Vespra DeFi swarm. \
You MUST respond with valid JSON only. No prose, no markdown. Base your analysis on LIVE_POOL_DATA.\n\n\
Output schema: { \"opportunities\": [ { \"protocol\": \"string\", \"pool\": \"string\", \
\"chain\": \"string\", \"apy\": float, \"tvl_usd\": int, \"momentum_score\": float, \
\"entry_signal\": \"strong|moderate|weak|none\", \"price_change_24h_pct\": float, \
\"risk_tier\": \"LOW|MEDIUM|HIGH\", \"recommended_action\": \"string\" } ], \
\"summary\": \"string\", \"top_chain\": \"string\", \"strong_signal_count\": int }\n\n\
Rules: No transactions, no keys. Analyze LIVE_POOL_DATA only. \
Return max 5 opportunities sorted by momentum_score descending.";

pub struct ScoutAgent {
    llm: Arc<dyn AgentClient>,
}

impl ScoutAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn analyze(&self, ctx: &ScoutContext) -> Result<ScoutDecision> {
        let pools_json = serde_json::to_string(&ctx.pools)?;
        let task = format!(
            "LIVE_POOL_DATA: {pools_json}\n\n\
             [TASK] Find momentum opportunities for wallet {} mode={}",
            ctx.wallet_id, ctx.mode
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        // Try to parse the full response object with "opportunities" array
        let opps = if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(arr) = val.get("opportunities").and_then(|v| v.as_array()) {
                arr.iter()
                    .filter_map(|item| parse_opportunity(item))
                    .collect::<Vec<_>>()
            } else if val.is_array() {
                // LLM returned a bare array
                val.as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|item| parse_opportunity(item))
                    .collect()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        if opps.is_empty() {
            Ok(ScoutDecision::NoOpportunities {
                reason: "no valid opportunities in LLM response".into(),
            })
        } else {
            Ok(ScoutDecision::Opportunities(opps))
        }
    }
}

/// Parse an Opportunity from a serde_json::Value, using defaults for missing fields.
fn parse_opportunity(item: &serde_json::Value) -> Option<Opportunity> {
    let protocol = item.get("protocol")?.as_str()?.to_string();
    let pool = item
        .get("pool")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chain = item
        .get("chain")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let apy = item.get("apy").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tvl_usd = item
        .get("tvl_usd")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let momentum_score = item
        .get("momentum_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let price_change_24h_pct = item
        .get("price_change_24h_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let price_usd = item
        .get("price_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let volume_24h = item
        .get("volume_24h")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let volume_spike_pct = item
        .get("volume_spike_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let tvl_change_7d_pct = item
        .get("tvl_change_7d_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let il_risk = item
        .get("il_risk")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let entry_signal = match item
        .get("entry_signal")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_lowercase()
        .as_str()
    {
        "strong" => EntrySignal::Strong,
        "moderate" => EntrySignal::Moderate,
        "weak" => EntrySignal::Weak,
        _ => EntrySignal::None,
    };

    let risk_tier = match item
        .get("risk_tier")
        .and_then(|v| v.as_str())
        .unwrap_or("HIGH")
        .to_uppercase()
        .as_str()
    {
        "LOW" => RiskTier::Low,
        "MEDIUM" => RiskTier::Medium,
        _ => RiskTier::High,
    };

    Some(Opportunity {
        protocol,
        pool,
        chain,
        apy,
        tvl_usd,
        momentum_score,
        entry_signal,
        price_change_24h_pct,
        price_usd,
        risk_tier,
        il_risk,
        volume_24h,
        volume_spike_pct,
        tvl_change_7d_pct,
    })
}
