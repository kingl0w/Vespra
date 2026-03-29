use redis::AsyncCommands;

use crate::error::GatewayError;

pub struct FeeEngine {
    _keymaster_url: String,
    _client: reqwest::Client,
    redis: std::sync::Arc<redis::Client>,
}

impl FeeEngine {
    pub fn new(keymaster_url: String, client: reqwest::Client, redis: std::sync::Arc<redis::Client>) -> Self {
        Self { _keymaster_url: keymaster_url, _client: client, redis }
    }

    pub async fn sweep_performance_fee(
        &self,
        wallet_id: &str,
        capital_eth: f64,
        strategy: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        // Record the fee event in Redis
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let fee_pct = 0.5; // 0.5% performance fee
            let fee_eth = capital_eth * fee_pct / 100.0;
            let entry = serde_json::json!({
                "wallet_id": wallet_id,
                "capital_eth": capital_eth,
                "fee_eth": fee_eth,
                "strategy": strategy,
                "timestamp": chrono::Utc::now().timestamp(),
            });
            if let Ok(json) = serde_json::to_string(&entry) {
                let _: Result<(), _> = conn.lpush::<_, _, ()>("vespra:fee_records", &json).await;
                let _: Result<(), _> = conn.ltrim::<_, ()>("vespra:fee_records", 0, 499).await;
                // Increment total
                let _: Result<(), _> = conn.incr::<_, _, ()>("vespra:fees:total_eth", fee_eth.to_string()).await;
            }
            Ok(serde_json::json!({ "fee_eth": fee_eth, "wallet_id": wallet_id }))
        } else {
            Ok(serde_json::json!({}))
        }
    }

    pub async fn fee_summary(&self) -> Result<serde_json::Value, GatewayError> {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let total: f64 = conn
                .get::<_, Option<String>>("vespra:fees:total_eth")
                .await
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);

            let records: Vec<String> = conn
                .lrange("vespra:fee_records", 0, 19)
                .await
                .unwrap_or_default();
            let recent: Vec<serde_json::Value> = records
                .iter()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect();

            Ok(serde_json::json!({
                "total_fees_eth": total,
                "recent_fees": recent,
            }))
        } else {
            Ok(serde_json::json!({"total_fees_eth": 0.0}))
        }
    }
}
