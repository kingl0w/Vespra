use crate::error::GatewayError;

pub struct FeeEngine {
    keymaster_url: String,
    client: reqwest::Client,
}

impl FeeEngine {
    pub fn new(keymaster_url: String, client: reqwest::Client) -> Self {
        Self { keymaster_url, client }
    }

    pub async fn sweep_performance_fee(
        &self,
        _wallet_id: &str,
        _capital_eth: f64,
        _strategy: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        Ok(serde_json::json!({}))
    }

    pub async fn fee_summary(&self) -> Result<serde_json::Value, GatewayError> {
        Ok(serde_json::json!({}))
    }
}
