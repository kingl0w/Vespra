use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::types::decisions::{SentinelAssessment, SentinelDecision};
use crate::types::trade_up::TradePosition;
use crate::types::wallet::WalletState;

#[derive(Debug, Clone, Serialize)]
pub struct SentinelContext {
    pub wallets: Vec<WalletState>,
    pub stop_loss_pct: f64,
}

#[derive(Debug, Deserialize)]
struct SentinelRaw {
    #[serde(default)]
    overall_status: Option<String>,
    #[serde(default)]
    stop_loss_triggered: Option<bool>,
    #[serde(default)]
    wallet_id: Option<String>,
    #[serde(default)]
    loss_pct: Option<f64>,
    #[serde(default)]
    message: Option<String>,
}

const SYSTEM_PROMPT: &str = "You are Sentinel, the portfolio watchdog of the Vespra DeFi swarm. \
You MUST respond with valid JSON only.\n\n\
Monitor wallet positions and health. Check for stop-loss conditions, \
abnormal balance changes, and position health.\n\n\
Output schema: { \"overall_status\": \"healthy|warning|critical\", \
\"stop_loss_triggered\": true|false, \
\"wallet_id\": \"<uuid if stop loss>\", \
\"loss_pct\": <float if stop loss>, \
\"message\": \"<details>\" }\n\n\
Rules:\n\
- If any wallet drawdown exceeds stop_loss_pct → overall_status=\"critical\", stop_loss_triggered=true\n\
- If balance decreased >10% in 24h → overall_status=\"warning\"\n\
- Otherwise → overall_status=\"healthy\"";

pub struct SentinelAgent {
    llm: Arc<dyn AgentClient>,
    keymaster_url: String,
    keymaster_token: String,
    http_client: reqwest::Client,
}

impl SentinelAgent {
    pub fn new(
        llm: Arc<dyn AgentClient>,
        keymaster_url: String,
        keymaster_token: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self { llm, keymaster_url, keymaster_token, http_client }
    }

    pub async fn check(&self, ctx: &SentinelContext) -> Result<SentinelDecision> {
        let task = serde_json::to_string(ctx)?;
        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let parsed: SentinelRaw = serde_json::from_str(&raw).unwrap_or(SentinelRaw {
            overall_status: None,
            stop_loss_triggered: None,
            wallet_id: None,
            loss_pct: None,
            message: Some(format!("parse_error: {raw}")),
        });

        let status = parsed
            .overall_status
            .as_deref()
            .unwrap_or("healthy")
            .to_lowercase();

        if status == "critical" || parsed.stop_loss_triggered.unwrap_or(false) {
            let wallet_id = parsed
                .wallet_id
                .as_deref()
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .unwrap_or_default();
            return Ok(SentinelDecision::StopLoss {
                wallet_id,
                loss_pct: parsed.loss_pct.unwrap_or(0.0),
            });
        }

        if status == "warning" {
            return Ok(SentinelDecision::Alert {
                message: parsed
                    .message
                    .unwrap_or_else(|| "warning condition".into()),
            });
        }

        Ok(SentinelDecision::Healthy)
    }

    pub async fn monitor_position(
        &self,
        position: &TradePosition,
        current_price: f64,
    ) -> Result<SentinelAssessment> {
        let gain_pct = if position.entry_price_usd > 0.0 {
            ((current_price - position.entry_price_usd) / position.entry_price_usd) * 100.0
        } else {
            0.0
        };

        let system = "You are Sentinel, the portfolio watchdog of the Vespra DeFi swarm. \
            You MUST respond with valid JSON only.\n\n\
            Assess the open position and decide whether to hold, exit for gain, or cut loss.\n\n\
            Output schema: { \"action\": \"hold\" | \"exit_gain\" | \"exit_loss\", \"reasoning\": \"<explanation>\" }\n\n\
            Rules:\n\
            - If gain >= 10% and momentum is fading → exit_gain\n\
            - If loss >= 8% or risk is escalating → exit_loss\n\
            - Otherwise → hold";

        let task = format!(
            "Monitor this open position: {} entered at {:.4} USD, current price {:.4} USD, \
             gain/loss: {:.2}%. Should we hold, exit for gain, or cut loss?",
            position.token_symbol, position.entry_price_usd, current_price, gain_pct,
        );

        let raw = self.llm.call(system, &task).await?;

        #[derive(Deserialize)]
        struct RawAssessment {
            #[serde(default = "default_hold")]
            action: String,
            #[serde(default)]
            reasoning: String,
        }
        fn default_hold() -> String { "hold".into() }

        let parsed: RawAssessment = serde_json::from_str(&raw).unwrap_or(RawAssessment {
            action: "hold".into(),
            reasoning: format!("parse_error: {}", &raw[..raw.len().min(200)]),
        });

        Ok(SentinelAssessment {
            action: parsed.action,
            reasoning: parsed.reasoning,
        })
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        let wallet_data = self
            .http_client
            .get(format!("{}/wallets", self.keymaster_url))
            .header("Authorization", format!("Bearer {}", self.keymaster_token))
            .send()
            .await
            .and_then(|r| Ok(r))
            .ok();

        let task = if let Some(resp) = wallet_data {
            let body = resp.text().await.unwrap_or_default();
            format!(
                "You are Sentinel. Here is the REAL current wallet data from Keymaster:\n\
                 {body}\n\n\
                 Answer this question using ONLY the data above. Do not invent wallet counts, \
                 balances, or portfolio values. If data is missing, say so explicitly.\n\n\
                 Question: {question}"
            )
        } else {
            format!(
                "WARNING: Could not fetch real wallet data. Answer based only on what you know \
                 from the system context, and explicitly state that real data is unavailable.\n\n\
                 Question: {question}"
            )
        };

        let prompt = format!(
            "{}\n\nHowever, for this request respond with helpful prose or JSON as appropriate. \
             Do not restrict yourself to the sentinel schema — answer the user's question directly.",
            SYSTEM_PROMPT
        );
        self.llm.call(&prompt, &task).await
    }
}
