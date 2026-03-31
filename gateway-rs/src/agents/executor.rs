use std::sync::Arc;

use anyhow::Result;

use crate::config::GatewayConfig;
use crate::types::decisions::{ExecutorResult, ExecutorStatus};

pub struct ExecutorAgent {
    keymaster_url: String,
    keymaster_token: String,
    client: reqwest::Client,
    config: Arc<GatewayConfig>,
}

impl ExecutorAgent {
    pub fn new(
        keymaster_url: String,
        keymaster_token: String,
        client: reqwest::Client,
        config: Arc<GatewayConfig>,
    ) -> Self {
        Self {
            keymaster_url,
            keymaster_token,
            client,
            config,
        }
    }

    pub async fn execute(
        &self,
        wallet_id: uuid::Uuid,
        _token_in: &str,
        token_out: &str,
        amount_wei: &str,
        chain: &str,
    ) -> Result<ExecutorResult> {
        let payload = serde_json::json!({
            "wallet_id": wallet_id,
            "to": token_out,
            "amount_eth": amount_wei,
            "chain": chain,
            "deadline": crate::guards::tx_deadline(&self.config),
            "rpc_url": self.config.rpc_url_override.as_deref().unwrap_or(""),
        });

        let resp = self
            .client
            .post(format!("{}/tx/send", self.keymaster_url))
            .header("Authorization", format!("Bearer {}", self.keymaster_token))
            .json(&payload)
            .send()
            .await?;

        let data: serde_json::Value = resp.json().await?;
        let tx_hash = data["tx_hash"].as_str().map(|s| s.to_string());

        Ok(ExecutorResult {
            status: if tx_hash.is_some() {
                ExecutorStatus::Success
            } else {
                ExecutorStatus::Failed
            },
            tx_hash,
            error: data["error"].as_str().map(|s| s.to_string()),
        })
    }
}
