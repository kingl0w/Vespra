use serde::{Deserialize, Serialize};

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

pub struct QuoteFetcher;

impl QuoteFetcher {
    pub fn new() -> Self {
        Self
    }

    /// Stub: returns a simulated quote. Real 1inch integration is Phase 6.
    pub async fn fetch_quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in_wei: &str,
        _chain_id: u64,
    ) -> anyhow::Result<SwapQuote> {
        Ok(SwapQuote {
            token_in: token_in.to_string(),
            token_out: token_out.to_string(),
            amount_in_wei: amount_in_wei.to_string(),
            amount_out_wei: amount_in_wei.to_string(), // 1:1 simulated
            price_impact: 0.5,
            gas_estimate: 200_000,
            route: "simulated".into(),
        })
    }
}
