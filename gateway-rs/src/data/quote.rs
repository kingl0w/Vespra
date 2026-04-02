use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParaSwapPriceResponse {
    #[serde(default)]
    price_route: Option<ParaSwapPriceRoute>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParaSwapPriceRoute {
    #[serde(default)]
    dest_amount: Option<String>,
    #[serde(default)]
    gas_cost: Option<String>,
}

pub struct QuoteFetcher {
    client: reqwest::Client,
    api_key: Option<String>,
    paraswap_mode: bool,
    chain_registry: Arc<ChainRegistry>,
}

impl QuoteFetcher {
    pub fn new(
        client: reqwest::Client,
        api_key: Option<String>,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self {
            client,
            api_key,
            paraswap_mode: false,
            chain_registry,
        }
    }

    pub fn from_config(
        client: reqwest::Client,
        config: &GatewayConfig,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        let paraswap = config.paraswap_mode;
        let has_key = config
            .oneinch_api_key
            .as_ref()
            .map(|k| !k.is_empty())
            .unwrap_or(false);

        if !has_key && !paraswap {
            tracing::warn!(
                "neither ONEINCH_API_KEY nor PARASWAP_MODE=true is set — \
                 quote fetcher will use simulated fallback quotes"
            );
        }

        if paraswap {
            tracing::info!("DEX routing: ParaSwap mode enabled (no KYC required)");
        } else if has_key {
            tracing::info!("DEX routing: 1inch mode (API key present)");
        }

        Self {
            client,
            api_key: config.oneinch_api_key.clone(),
            paraswap_mode: paraswap,
            chain_registry,
        }
    }

    pub async fn fetch_quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in_wei: &str,
        chain_id: u64,
    ) -> anyhow::Result<SwapQuote> {
        if self.paraswap_mode {
            return self
                .fetch_paraswap_quote(token_in, token_out, amount_in_wei, chain_id)
                .await;
        }

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

        let price_impact = calc_price_impact(amount_in_wei, &amount_out_wei);

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

    async fn fetch_paraswap_quote(
        &self,
        token_in: &str,
        token_out: &str,
        amount_in_wei: &str,
        chain_id: u64,
    ) -> anyhow::Result<SwapQuote> {
        let url = format!(
            "https://apiv5.paraswap.io/prices\
             ?srcToken={token_in}&destToken={token_out}&amount={amount_in_wei}\
             &network={chain_id}&srcDecimals=18&destDecimals=18&side=SELL"
        );

        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("paraswap quote request failed: {e} — using fallback");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        let status = resp.status();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("paraswap quote read body failed: {e} — using fallback");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        if !status.is_success() {
            tracing::warn!(
                "paraswap quote returned {status}: {}",
                &body[..body.len().min(300)]
            );
            return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
        }

        let parsed: ParaSwapPriceResponse = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("paraswap quote parse error: {e} — using fallback");
                return Ok(self.fallback_quote(token_in, token_out, amount_in_wei));
            }
        };

        let route = parsed.price_route.unwrap_or(ParaSwapPriceRoute {
            dest_amount: None,
            gas_cost: None,
        });

        let amount_out_wei = route
            .dest_amount
            .unwrap_or_else(|| amount_in_wei.to_string());
        let gas_estimate: u64 = route
            .gas_cost
            .and_then(|g| g.parse().ok())
            .unwrap_or(200_000);

        let price_impact = calc_price_impact(amount_in_wei, &amount_out_wei);

        tracing::info!(
            "paraswap quote: {} → {} amount_out={} gas={} impact={:.2}%",
            token_in, token_out, amount_out_wei, gas_estimate, price_impact
        );

        Ok(SwapQuote {
            token_in: token_in.to_string(),
            token_out: token_out.to_string(),
            amount_in_wei: amount_in_wei.to_string(),
            amount_out_wei,
            price_impact,
            gas_estimate,
            route: "paraswap_v5".into(),
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

fn calc_price_impact(amount_in_wei: &str, amount_out_wei: &str) -> f64 {
    let in_val: f64 = amount_in_wei.parse().unwrap_or(1.0);
    let out_val: f64 = amount_out_wei.parse().unwrap_or(1.0);
    if in_val > 0.0 {
        ((in_val - out_val) / in_val * 100.0).abs()
    } else {
        0.0
    }
}
