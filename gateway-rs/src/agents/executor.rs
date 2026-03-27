use anyhow::Result;

use crate::types::decisions::{ExecutorResult, ExecutorStatus};

pub struct ExecutorAgent {
    keymaster_url: String,
    keymaster_token: String,
    client: reqwest::Client,
}

impl ExecutorAgent {
    pub fn new(keymaster_url: String, keymaster_token: String, client: reqwest::Client) -> Self {
        Self {
            keymaster_url,
            keymaster_token,
            client,
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
