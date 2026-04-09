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
        token_in: &str,
        token_out: &str,
        amount_wei: &str,
        chain: &str,
    ) -> Result<ExecutorResult> {
        let payload = serde_json::json!({
            "wallet_id": wallet_id,
            "token_in": token_in,
            "token_out": token_out,
            "amount_in_wei": amount_wei,
            "chain": chain,
        });

        let _ = crate::guards::tx_deadline(&self.config);
        let _ = self.config.rpc_url_override.as_deref();

        let resp = self
            .client
            .post(format!("{}/swap", self.keymaster_url))
            .header("Authorization", format!("Bearer {}", self.keymaster_token))
            .json(&payload)
            .send()
            .await?;

        let status_code = resp.status();
        let data: serde_json::Value = resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
        let tx_hash = data["tx_hash"].as_str().map(|s| s.to_string());
        let error = data["error"].as_str().map(|s| s.to_string()).or_else(|| {
            if !status_code.is_success() {
                Some(format!("keymaster returned {status_code}"))
            } else {
                None
            }
        });

        //surface wrap/approve hashes via tracing so they're visible alongside
        //the existing exec-trace logs without changing the executorresult shape.
        if let Some(wrap) = data["wrap_tx_hash"].as_str() {
            tracing::info!("[exec-trace] swap wrap tx={}", wrap);
        }
        if let Some(approve) = data["approve_tx_hash"].as_str() {
            tracing::info!("[exec-trace] swap approve tx={}", approve);
        }

        Ok(ExecutorResult {
            status: if tx_hash.is_some() {
                ExecutorStatus::Success
            } else {
                ExecutorStatus::Failed
            },
            tx_hash,
            error,
        })
    }
}
