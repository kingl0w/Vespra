use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::chain::ChainRegistry;
use crate::types::wallet::WalletState;

#[derive(Debug, Deserialize)]
struct KeymasterWallet {
    #[serde(default)]
    wallet_id: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    chain: String,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<String>,
}

pub struct WalletFetcher {
    keymaster_url: String,
    keymaster_token: String,
    client: reqwest::Client,
    chain_registry: Arc<ChainRegistry>,
}

impl WalletFetcher {
    pub fn new(
        keymaster_url: String,
        keymaster_token: String,
        client: reqwest::Client,
        chain_registry: Arc<ChainRegistry>,
    ) -> Self {
        Self {
            keymaster_url,
            keymaster_token,
            client,
            chain_registry,
        }
    }

    pub async fn fetch_wallets(&self, chain: &str) -> Result<Vec<WalletState>> {
        let url = format!("{}/wallets?chain={chain}", self.keymaster_url);

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&self.keymaster_token)
            .send()
            .await
            .context("failed to fetch wallets from keymaster")?;

        let status = resp.status();
        let body = resp.text().await.context("failed to read keymaster response body")?;

        if !status.is_success() {
            tracing::warn!("keymaster /wallets returned {status}: {}", &body[..body.len().min(200)]);
            return Ok(vec![]);
        }

        let entries: Vec<KeymasterWallet> = serde_json::from_str(&body)
            .context("failed to parse keymaster wallets JSON")?;

        // Look up RPC URL from chain registry
        let rpc_url = self
            .chain_registry
            .get(chain)
            .map(|c| c.rpc_url.as_str())
            .unwrap_or("");

        let mut wallets = Vec::with_capacity(entries.len());

        for entry in entries {
            let wallet_id = uuid::Uuid::parse_str(&entry.wallet_id).unwrap_or_default();
            let address = entry.address.clone();
            let wallet_chain = if entry.chain.is_empty() {
                chain.to_string()
            } else {
                entry.chain
            };

            // Fetch ETH balance via JSON-RPC if we have an RPC URL
            let balance_eth = if !rpc_url.is_empty() && !address.is_empty() {
                match self.fetch_eth_balance(rpc_url, &address).await {
                    Ok(bal) => {
                        tracing::debug!("wallet {} ({}) balance: {:.6} ETH", wallet_id, address, bal);
                        bal
                    }
                    Err(e) => {
                        tracing::warn!("eth_getBalance failed for {}: {}", address, e);
                        0.0
                    }
                }
            } else {
                if rpc_url.is_empty() {
                    tracing::debug!("no rpc_url for chain '{chain}', skipping balance fetch");
                }
                0.0
            };

            wallets.push(WalletState {
                wallet_id,
                address,
                chain: wallet_chain,
                balance_eth,
                token_positions: vec![],
                alerts: vec![],
            });
        }

        tracing::info!(
            "fetched {} wallets for chain '{}' (rpc={})",
            wallets.len(),
            chain,
            !rpc_url.is_empty()
        );

        Ok(wallets)
    }

    async fn fetch_eth_balance(&self, rpc_url: &str, address: &str) -> Result<f64> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "eth_getBalance",
            "params": [address, "latest"],
            "id": 1
        });

        let resp = self
            .client
            .post(rpc_url)
            .json(&payload)
            .send()
            .await
            .context("eth_getBalance request failed")?
            .json::<RpcResponse>()
            .await
            .context("failed to parse eth_getBalance response")?;

        let hex_str = resp
            .result
            .ok_or_else(|| anyhow::anyhow!("null result from eth_getBalance"))?;

        // Parse hex string (0x...) to u128 then to f64 ETH
        let hex_str = hex_str.trim_start_matches("0x");
        let wei = u128::from_str_radix(hex_str, 16)
            .context("failed to parse hex balance")?;

        Ok(wei as f64 / 1e18)
    }
}
