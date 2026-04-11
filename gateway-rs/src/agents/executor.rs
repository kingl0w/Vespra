use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::config::GatewayConfig;
use crate::types::decisions::{ExecutorResult, ExecutorStatus};

//ves-fix: transient-error retry policy for the keymaster BUY call. only
//connection/timeout errors retry — HTTP 4xx/5xx responses are permanent.
const KEYMASTER_MAX_ATTEMPTS: u32 = 3;
const KEYMASTER_RETRY_DELAY_SECS: u64 = 2;

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

        let mut last_conn_error: Option<String> = None;

        for attempt in 1..=KEYMASTER_MAX_ATTEMPTS {
            let send_result = self
                .client
                .post(format!("{}/swap", self.keymaster_url))
                .header("Authorization", format!("Bearer {}", self.keymaster_token))
                .json(&payload)
                .send()
                .await;

            let resp = match send_result {
                Ok(r) => r,
                Err(e) if e.is_connect() || e.is_timeout() => {
                    //ves-fix: transient transport failure — retry without
                    //leaking the upstream URL or reqwest internals.
                    tracing::warn!(
                        "Keymaster BUY attempt {}/{} failed with connection error, retrying...",
                        attempt, KEYMASTER_MAX_ATTEMPTS
                    );
                    last_conn_error = Some(format!(
                        "swap service temporarily unavailable (attempt {}/{} failed)",
                        attempt, KEYMASTER_MAX_ATTEMPTS
                    ));
                    if attempt < KEYMASTER_MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_secs(KEYMASTER_RETRY_DELAY_SECS)).await;
                        continue;
                    }
                    return Ok(ExecutorResult {
                        status: ExecutorStatus::Failed,
                        tx_hash: None,
                        error: last_conn_error,
                    });
                }
                Err(_) => {
                    //ves-fix: non-transient transport failure (e.g. body/decode
                    //error). do not retry and do not surface reqwest internals.
                    return Ok(ExecutorResult {
                        status: ExecutorStatus::Failed,
                        tx_hash: None,
                        error: Some("swap service request failed".to_string()),
                    });
                }
            };

            let status_code = resp.status();
            let data: serde_json::Value =
                resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
            let tx_hash = data["tx_hash"].as_str().map(|s| s.to_string());
            //ves-fix: preserve keymaster response body on http failures (useful
            //operator context) but never include URLs or transport details.
            let error = data["error"].as_str().map(|s| s.to_string()).or_else(|| {
                if !status_code.is_success() {
                    Some(format!("keymaster returned {status_code}"))
                } else {
                    None
                }
            });

            if let Some(wrap) = data["wrap_tx_hash"].as_str() {
                tracing::info!("[exec-trace] swap wrap tx={}", wrap);
            }
            if let Some(approve) = data["approve_tx_hash"].as_str() {
                tracing::info!("[exec-trace] swap approve tx={}", approve);
            }

            return Ok(ExecutorResult {
                status: if tx_hash.is_some() {
                    ExecutorStatus::Success
                } else {
                    ExecutorStatus::Failed
                },
                tx_hash,
                error,
            });
        }

        //unreachable: the loop either returns or retries up to the cap, but
        //keep a safe fallback in case the control flow is ever refactored.
        Ok(ExecutorResult {
            status: ExecutorStatus::Failed,
            tx_hash: None,
            error: last_conn_error
                .or_else(|| Some("swap service temporarily unavailable".to_string())),
        })
    }
}
