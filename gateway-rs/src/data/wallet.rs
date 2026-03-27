use anyhow::{Context, Result};
use serde::Deserialize;

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

pub struct WalletFetcher {
    keymaster_url: String,
    keymaster_token: String,
    client: reqwest::Client,
}

impl WalletFetcher {
    pub fn new(keymaster_url: String, keymaster_token: String, client: reqwest::Client) -> Self {
        Self {
            keymaster_url,
            keymaster_token,
            client,
        }
    }

    pub async fn fetch_wallets(&self, chain: &str) -> Result<Vec<WalletState>> {
        let url = format!("{}/wallets?chain={chain}", self.keymaster_url);

        let resp = self.client
            .get(&url)
            .bearer_auth(&self.keymaster_token)
            .send()
            .await
            .context("failed to fetch wallets from keymaster")?
            .json::<Vec<KeymasterWallet>>()
            .await
            .context("failed to parse keymaster wallets response")?;

        let wallets = resp
            .into_iter()
            .map(|w| WalletState {
                wallet_id: uuid::Uuid::parse_str(&w.wallet_id).unwrap_or_default(),
                address: w.address,
                chain: w.chain,
                balance_eth: 0.0, // populated by RPC in orchestrator layer
                token_positions: vec![],
                alerts: vec![],
            })
            .collect();

        Ok(wallets)
    }
}
