use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chain::ChainRegistry;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SwapQuote {
    pub token_in: String,
    pub token_out: String,
    pub amount_in_wei: String,
    pub amount_out_wei: String,
    pub price_impact: f64,
    pub gas_estimate: u64,
    pub route: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OneInchQuoteResponse {
    #[serde(default)]
    dst_amount: Option<String>,
    #[serde(default)]
    gas: Option<u64>,
}

pub struct QuoteFetcher {
    client: reqwest::Client,
    api_key: Option<String>,
    chain_registry: Arc<ChainRegistry>,
}

impl QuoteFetcher {
    pub fn new(
        client: reqwest::Client,
        api_key: Option<String>,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self { client, api_key, chain_registry }
    }

    pub async fn fetch_quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in_wei: &str,
        chain_id: u64,
    ) -> anyhow::Result<SwapQuote> {
        let api_key = match &self.api_key {
            Some(k) if !k.is_empty() => k,
            _ => {
                tracing::warn!("no 1inch API key — using simulated fallback quote");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        let url = format!(
            "https://api.1inch.dev/swap/v6.0/{chain_id}/quote\
             ?src={token_in}&dst={token_out}&amount={amount_in_wei}&includeGas=true"
        );

        let resp = match self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("1inch quote request failed: {e} — using fallback");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        let status = resp.status();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("1inch quote read body failed: {e} — using fallback");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        if !status.is_success() {
            tracing::warn!(
                "1inch quote returned {status}: {}",
                &body[..body.len().min(300)]
            );
            return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
        }

        let parsed: OneInchQuoteResponse = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("1inch quote parse error: {e} — using fallback");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        let amount_out_wei = parsed.dst_amount.unwrap_or_else(|| amount_in_wei.to_string());
        let gas_estimate = parsed.gas.unwrap_or(200_000);

        // Calculate price impact from amounts
        let in_val: f64 = amount_in_wei.parse().unwrap_or(1.0);
        let out_val: f64 = amount_out_wei.parse().unwrap_or(1.0);
        let price_impact = if in_val > 0.0 {
            ((in_val - out_val) / in_val * 100.0).abs()
        } else {
            0.0
        };

        tracing::info!(
            "1inch quote: {} → {} amount_out={} gas={} impact={:.2}%",
            token_in, token_out, amount_out_wei, gas_estimate, price_impact
        );

        Ok(SwapQuote {
            token_in: token_in.to_string(),
            token_out: token_out.to_string(),
            amount_in_wei: amount_in_wei.to_string(),
            amount_out_wei,
            price_impact,
            gas_estimate,
            route: "1inch_v6".into(),
        })
    }

    fn fallback_quote(&self, token_in: &str, token_out: &str, amount_in_wei: &str) -> SwapQuote {
        SwapQuote {
            token_in: token_in.to_string(),
            token_out: token_out.to_string(),
            amount_in_wei: amount_in_wei.to_string(),
            amount_out_wei: amount_in_wei.to_string(),
            price_impact: 0.5,
            gas_estimate: 200_000,
            route: "simulated".into(),
        }
    }
}
