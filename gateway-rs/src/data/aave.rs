use std::sync::Arc;

use anyhow::{Context, Result};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

//── types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AavePosition {
    pub protocol: String,
    pub chain: String,
    pub asset: String,
    pub supplied: f64,
    pub borrowed: f64,
    pub supply_apy: f64,
    pub borrow_apy: f64,
    pub health_factor: Option<f64>,
    pub net_apy: f64,
    pub gas_drag_apy: f64,
}

//── subgraph url mapping ─────────────────────────────────────────

///known aave v3 subgraph endpoints per chain.
fn subgraph_url(chain: &str) -> Option<&'static str> {
    match chain {
        "base" => Some(
            "https://gateway.thegraph.com/api/public/subgraphs/id/GQFbb95cE6d8mV989mL5figjaGaKCQB3xqYrr1bRyXqF",
        ),
        "ethereum" => Some(
            "https://gateway.thegraph.com/api/public/subgraphs/id/Cd2gEDVeqnjBn1hSeqFMitw8Q1iiyV9FYUZkLNRcL87g",
        ),
        "arbitrum" => Some(
            "https://gateway.thegraph.com/api/public/subgraphs/id/DLuE98AEBRGDzwCnaoGgTfQVSfYQrksRDBLL5ePsShaB",
        ),
        "optimism" => Some(
            "https://gateway.thegraph.com/api/public/subgraphs/id/DSfLz8oQBUeU5atALgUFQKMTSYV9mZAVYp4noLSXAfvb",
        ),
        "polygon" => Some(
            "https://gateway.thegraph.com/api/public/subgraphs/id/Co2URyXjnxaw8WqxKyVHdirq9Ahhm5vcTs4dMedAq211",
        ),
        _ => None,
    }
}

//── subgraph response types ──────────────────────────────────────

#[derive(Deserialize)]
struct SubgraphResponse {
    data: Option<SubgraphData>,
}

#[derive(Deserialize)]
struct SubgraphData {
    #[serde(rename = "userReserves", default)]
    user_reserves: Vec<SubgraphUserReserve>,
}

#[derive(Deserialize)]
struct SubgraphUserReserve {
    #[serde(rename = "currentATokenBalance", default)]
    current_a_token_balance: String,
    #[serde(rename = "currentVariableDebt", default)]
    current_variable_debt: String,
    reserve: SubgraphReserve,
}

#[derive(Deserialize)]
struct SubgraphReserve {
    symbol: String,
    decimals: i32,
    #[serde(rename = "liquidityRate", default)]
    liquidity_rate: String,
    #[serde(rename = "variableBorrowRate", default)]
    variable_borrow_rate: String,
}

//── config for gas drag calculation ──────────────────────────────

///estimated gas cost for one aave deposit/withdraw in eth.
///used for gas drag apy calculation.
const GAS_COST_PER_TX_ETH: f64 = 0.0003;
///assumed number of yield-rotation transactions per month.
const TXS_PER_MONTH: f64 = 2.0;
const ETH_PRICE_USD: f64 = 3_000.0;

//── fetcher ──────────────────────────────────────────────────────

pub struct AaveFetcher {
    client: reqwest::Client,
    redis: Arc<redis::Client>,
}

impl AaveFetcher {
    pub fn new(client: reqwest::Client, redis: Arc<redis::Client>) -> Self {
        Self { client, redis }
    }

    ///fetch all open aave v3 positions for `wallet_address` on `chain`.
    pub async fn fetch_positions(
        &self,
        chain: &str,
        wallet_address: &str,
    ) -> Result<Vec<AavePosition>> {
        let cache_key = format!(
            "vespra:aave_positions:{}:{}",
            chain,
            wallet_address.to_lowercase()
        );

        //check redis cache (120s ttl)
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let cached: Option<String> = conn.get(&cache_key).await.ok().flatten();
            if let Some(data) = cached {
                if let Ok(positions) = serde_json::from_str::<Vec<AavePosition>>(&data) {
                    return Ok(positions);
                }
            }
        }

        let url = subgraph_url(chain)
            .ok_or_else(|| anyhow::anyhow!("no Aave V3 subgraph for chain '{chain}'"))?;

        let addr_lower = wallet_address.to_lowercase();

        let query = serde_json::json!({
            "query": format!(
                r#"{{
                    userReserves(
                        where: {{
                            user: "{addr_lower}"
                        }}
                    ) {{
                        currentATokenBalance
                        currentVariableDebt
                        reserve {{
                            symbol
                            decimals
                            liquidityRate
                            variableBorrowRate
                        }}
                    }}
                }}"#
            )
        });

        let resp = self
            .client
            .post(url)
            .json(&query)
            .send()
            .await
            .context("aave subgraph request failed")?;

        let body: SubgraphResponse = resp
            .json()
            .await
            .context("failed to parse aave subgraph response")?;

        let reserves = body
            .data
            .map(|d| d.user_reserves)
            .unwrap_or_default();

        let mut positions = Vec::new();

        for ur in reserves {
            let decimals = ur.reserve.decimals as u32;
            let divisor = 10f64.powi(decimals as i32);

            let supplied_raw: f64 = ur
                .current_a_token_balance
                .parse::<f64>()
                .unwrap_or(0.0);
            let borrowed_raw: f64 = ur
                .current_variable_debt
                .parse::<f64>()
                .unwrap_or(0.0);

            let supplied = supplied_raw / divisor;
            let borrowed = borrowed_raw / divisor;

            //skip positions with no supply and no debt
            if supplied < 1e-8 && borrowed < 1e-8 {
                continue;
            }

            //rates are in ray units (1e27)
            let ray = 1e27;
            let supply_apy = ur
                .reserve
                .liquidity_rate
                .parse::<f64>()
                .unwrap_or(0.0)
                / ray
                * 100.0; // convert to percentage

            let borrow_apy = ur
                .reserve
                .variable_borrow_rate
                .parse::<f64>()
                .unwrap_or(0.0)
                / ray
                * 100.0;

            //net apy calculation
            let net_apy = if borrowed > 1e-8 && supplied > 1e-8 {
                supply_apy - (borrow_apy * (borrowed / supplied))
            } else {
                supply_apy
            };

            //gas drag: amortize 2 txs/month of gas cost over 30 days,
            //expressed as annualized apy drag relative to position value.
            let monthly_gas_cost_usd = GAS_COST_PER_TX_ETH * ETH_PRICE_USD * TXS_PER_MONTH;
            let position_value_usd = supplied * ETH_PRICE_USD; // rough estimate
            let gas_drag_apy = if position_value_usd > 1.0 {
                (monthly_gas_cost_usd * 12.0) / position_value_usd * 100.0
            } else {
                0.0
            };

            positions.push(AavePosition {
                protocol: "aave_v3".into(),
                chain: chain.to_string(),
                asset: ur.reserve.symbol,
                supplied,
                borrowed,
                supply_apy,
                borrow_apy,
                health_factor: None, // populated if keymaster supports it
                net_apy,
                gas_drag_apy,
            });
        }

        //cache in redis with 120s ttl
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            if let Ok(json) = serde_json::to_string(&positions) {
                let _: Result<(), _> = conn.set_ex(&cache_key, &json, 120).await;
            }
        }

        Ok(positions)
    }

    ///convenience: fetch positions and try to enrich with health factor
    ///from keymaster's wallet endpoint (if available).
    pub async fn fetch_positions_enriched(
        &self,
        chain: &str,
        wallet_address: &str,
        keymaster_url: &str,
        keymaster_token: &str,
        http_client: &reqwest::Client,
    ) -> Result<Vec<AavePosition>> {
        let mut positions = self.fetch_positions(chain, wallet_address).await?;

        //try to fetch health factor from keymaster
        let health_factor = fetch_health_factor(
            http_client,
            keymaster_url,
            keymaster_token,
            chain,
            wallet_address,
        )
        .await
        .ok()
        .flatten();

        if let Some(hf) = health_factor {
            for pos in &mut positions {
                pos.health_factor = Some(hf);
            }
        }

        Ok(positions)
    }
}

///try to get aave v3 health factor via keymaster's account data endpoint.
async fn fetch_health_factor(
    client: &reqwest::Client,
    keymaster_url: &str,
    keymaster_token: &str,
    chain: &str,
    wallet_address: &str,
) -> Result<Option<f64>> {
    #[derive(Deserialize)]
    struct AccountData {
        #[serde(rename = "healthFactor")]
        health_factor: Option<String>,
    }

    let url = format!(
        "{}/aave/account-data/{}/{}",
        keymaster_url.trim_end_matches('/'),
        chain,
        wallet_address
    );

    let resp = client
        .get(&url)
        .bearer_auth(keymaster_token)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let data: AccountData = r.json().await.unwrap_or(AccountData {
                health_factor: None,
            });
            match data.health_factor {
                Some(hf_str) => {
                    //health factor from contract is in 1e18 units
                    let hf = hf_str.parse::<f64>().unwrap_or(0.0) / 1e18;
                    if hf > 0.0 {
                        Ok(Some(hf))
                    } else {
                        Ok(None)
                    }
                }
                None => Ok(None),
            }
        }
        _ => Ok(None), // Keymaster may not support this endpoint yet
    }
}

///resolve a wallet label or id to an address via keymaster.
pub async fn resolve_wallet_address(
    client: &reqwest::Client,
    keymaster_url: &str,
    keymaster_token: &str,
    wallet_label: &str,
    chain: &str,
) -> Result<String> {
    #[derive(Deserialize)]
    struct KmWallet {
        #[serde(default)]
        address: String,
        #[serde(default)]
        label: String,
        #[serde(default)]
        wallet_id: String,
        #[serde(default)]
        chain: String,
    }

    let url = format!("{}/wallets", keymaster_url.trim_end_matches('/'));

    let wallets: Vec<KmWallet> = client
        .get(&url)
        .bearer_auth(keymaster_token)
        .send()
        .await
        .context("failed to fetch wallets from keymaster")?
        .json()
        .await
        .context("failed to parse keymaster wallets")?;

    //match by label or wallet_id, optionally filtered by chain
    let wallet_lower = wallet_label.to_lowercase();
    let chain_lower = chain.to_lowercase();

    let found = wallets
        .iter()
        .find(|w| {
            let chain_match = chain_lower.is_empty()
                || w.chain.to_lowercase() == chain_lower;
            let id_match = w.label.to_lowercase() == wallet_lower
                || w.wallet_id.to_lowercase() == wallet_lower;
            chain_match && id_match && !w.address.is_empty()
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "wallet '{}' not found on chain '{}'",
                wallet_label,
                chain
            )
        })?;

    Ok(found.address.clone())
}
