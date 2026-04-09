use std::time::Duration;

use serde::Serialize;

use crate::agents::executor::ExecutorAgent;
use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;
use crate::data::quote::QuoteFetcher;
use crate::types::tx::TxStatus;

const RECEIPT_TIMEOUT_SECS: u64 = 60;
const RECEIPT_POLL_INTERVAL_SECS: u64 = 3;

//── instrumented execution ──────────────────────────────────────

///execute a swap through the full traced path:
///quote → decision → calldata → keymaster → receipt polling
pub async fn execute_traced(
    executor: &ExecutorAgent,
    _config: &GatewayConfig,
    chain_registry: &ChainRegistry,
    wallet_id: uuid::Uuid,
    token_in: &str,
    token_out: &str,
    amount_wei: &str,
    chain: &str,
    dry_run: bool,
) -> TxStatus {
    //── step 3: build calldata ──────────────────────────────────
    let calldata = serde_json::json!({
        "wallet_id": wallet_id.to_string(),
        "to": token_out,
        "amount_eth": amount_wei,
        "chain": chain,
        "token_in": token_in,
        "token_out": token_out,
    });

    tracing::info!(
        "[exec-trace] calldata built: to={} value={} chain={}",
        token_out,
        amount_wei,
        chain
    );

    //── step 4: dry run gate ────────────────────────────────────
    if dry_run {
        let calldata_str = match serde_json::to_string(&calldata) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[exec-trace] failed to serialize dry-run calldata: {e}");
                return TxStatus::Failed {
                    error: format!("failed to serialize dry-run calldata: {e}"),
                };
            }
        };
        tracing::info!(
            "[DRY RUN] calldata ready, skipping broadcast: {}",
            calldata_str
        );
        return TxStatus::DryRun { calldata };
    }

    //── step 4: post to keymaster /tx/send ──────────────────────
    tracing::info!(
        "[exec-trace] POST keymaster /tx/send: wallet={} {} → {} amount={} chain={}",
        wallet_id,
        token_in,
        token_out,
        amount_wei,
        chain
    );

    let exec_result = match executor
        .execute(wallet_id, token_in, token_out, amount_wei, chain)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("[exec-trace] keymaster call failed: {e}");
            return TxStatus::Failed {
                error: format!("keymaster error: {e}"),
            };
        }
    };

    //── step 5: tx_hash returned ────────────────────────────────
    let tx_hash = match exec_result.tx_hash {
        Some(h) => {
            tracing::info!("[exec-trace] keymaster returned tx_hash={}", h);
            h
        }
        None => {
            let err = exec_result
                .error
                .unwrap_or_else(|| "no tx_hash returned".into());
            tracing::error!("[exec-trace] keymaster failed: {err}");
            return TxStatus::Failed { error: err };
        }
    };

    //── step 6: poll rpc for receipt ────────────────────────────
    let rpc_url = chain_registry
        .get(&chain.to_lowercase())
        .map(|c| c.rpc_url.as_str())
        .unwrap_or("");

    if rpc_url.is_empty() {
        tracing::error!(
            "[exec-trace] no RPC URL for chain={} — cannot confirm tx={}",
            chain,
            tx_hash
        );
        return TxStatus::Failed {
            error: format!("RPC URL not configured for chain '{chain}'"),
        };
    }

    let client = reqwest::Client::new();
    let max_attempts = (RECEIPT_TIMEOUT_SECS / RECEIPT_POLL_INTERVAL_SECS) as u32;

    for attempt in 1..=max_attempts {
        tracing::info!(
            "[exec-trace] polling receipt attempt {}/{} tx={}",
            attempt,
            max_attempts,
            tx_hash
        );

        let receipt = fetch_receipt(&client, rpc_url, &tx_hash).await;
        match receipt {
            Ok(Some(r)) => {
                let block = r.block_number;
                let gas = r.gas_used;
                //── step 7: receipt received ────────────────────
                if r.status == 1 {
                    tracing::info!(
                        "[exec-trace] tx CONFIRMED: hash={} block={} gas_used={}",
                        tx_hash,
                        block,
                        gas
                    );
                    return TxStatus::Confirmed {
                        tx_hash,
                        block_number: block,
                        gas_used: gas,
                    };
                } else {
                    tracing::error!(
                        "[exec-trace] tx REVERTED: hash={} block={} gas_used={}",
                        tx_hash,
                        block,
                        gas
                    );
                    return TxStatus::Reverted {
                        tx_hash,
                        block_number: block,
                        gas_used: gas,
                    };
                }
            }
            Ok(None) => {
                //receipt not yet available
            }
            Err(e) => {
                tracing::warn!("[exec-trace] receipt poll error: {e}");
            }
        }

        tokio::time::sleep(Duration::from_secs(RECEIPT_POLL_INTERVAL_SECS)).await;
    }

    tracing::warn!(
        "[exec-trace] receipt TIMEOUT after {}s for tx={}",
        RECEIPT_TIMEOUT_SECS,
        tx_hash
    );
    TxStatus::Timeout {
        tx_hash,
        attempts: max_attempts,
    }
}

//── rpc receipt fetcher ─────────────────────────────────────────

struct ReceiptData {
    block_number: u64,
    gas_used: u64,
    status: u64,
}

async fn fetch_receipt(
    client: &reqwest::Client,
    rpc_url: &str,
    tx_hash: &str,
) -> anyhow::Result<Option<ReceiptData>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getTransactionReceipt",
        "params": [tx_hash],
        "id": 1
    });

    let resp = client
        .post(rpc_url)
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    let data: serde_json::Value = resp.json().await?;
    let result = &data["result"];

    if result.is_null() {
        return Ok(None);
    }

    let block_hex = result["blockNumber"].as_str().unwrap_or("0x0");
    let gas_hex = result["gasUsed"].as_str().unwrap_or("0x0");
    let status_hex = result["status"].as_str().unwrap_or("0x0");

    Ok(Some(ReceiptData {
        block_number: u64::from_str_radix(block_hex.trim_start_matches("0x"), 16).unwrap_or(0),
        gas_used: u64::from_str_radix(gas_hex.trim_start_matches("0x"), 16).unwrap_or(0),
        status: u64::from_str_radix(status_hex.trim_start_matches("0x"), 16).unwrap_or(0),
    }))
}

//── validation checklist ────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ValidationCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

pub async fn run_validation_checks(
    config: &GatewayConfig,
    chain_registry: &ChainRegistry,
    quote_fetcher: &QuoteFetcher,
) -> Vec<ValidationCheck> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let mut checks = Vec::new();

    //1. keymaster reachable
    checks.push(check_keymaster(&client, &config.keymaster_url).await);

    //2. rpc reachable
    checks.push(check_rpc(&client, chain_registry).await);

    //3. 1inch / quote api reachable
    checks.push(check_quote_api(quote_fetcher, chain_registry).await);

    //4. wallet has balance > 0
    checks.push(
        check_wallet_balance(
            &client,
            &config.keymaster_url,
            &config.keymaster_token,
            chain_registry,
        )
        .await,
    );

    //5. gas estimate succeeds
    checks.push(check_gas_estimate(&client, chain_registry).await);

    checks
}

async fn check_keymaster(client: &reqwest::Client, keymaster_url: &str) -> ValidationCheck {
    let name = "keymaster_reachable".to_string();
    match client
        .get(format!("{}/health", keymaster_url))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => ValidationCheck {
            name,
            ok: true,
            detail: "Keymaster /health returned 200".into(),
        },
        Ok(r) => ValidationCheck {
            name,
            ok: false,
            detail: format!("Keymaster /health returned {}", r.status()),
        },
        Err(e) => ValidationCheck {
            name,
            ok: false,
            detail: format!("Keymaster unreachable: {e}"),
        },
    }
}

async fn check_rpc(client: &reqwest::Client, chain_registry: &ChainRegistry) -> ValidationCheck {
    let name = "rpc_reachable".to_string();

    let chain = chain_registry.available().into_iter().next();
    let chain = match chain {
        Some(c) => c,
        None => {
            return ValidationCheck {
                name,
                ok: false,
                detail: "No chains with RPC URLs configured".into(),
            }
        }
    };

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_blockNumber",
        "params": [],
        "id": 1
    });

    match client.post(&chain.rpc_url).json(&body).send().await {
        Ok(r) => {
            let data: serde_json::Value = r.json().await.unwrap_or_default();
            let block = data["result"].as_str().unwrap_or("0x0");
            let block_num =
                u64::from_str_radix(block.trim_start_matches("0x"), 16).unwrap_or(0);
            ValidationCheck {
                name,
                ok: block_num > 0,
                detail: format!("{}: block {}", chain.name, block_num),
            }
        }
        Err(e) => ValidationCheck {
            name,
            ok: false,
            detail: format!("RPC call failed on {}: {e}", chain.name),
        },
    }
}

async fn check_quote_api(
    quote_fetcher: &QuoteFetcher,
    chain_registry: &ChainRegistry,
) -> ValidationCheck {
    let name = "quote_api_reachable".to_string();

    let chain_id = chain_registry
        .available()
        .first()
        .map(|c| c.chain_id)
        .unwrap_or(8453);

    let amount = "1000000000000000"; // 0.001 ETH in wei
    let usdc = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"; // USDC on Base

    match quote_fetcher
        .fetch_quote("WETH", usdc, amount, chain_id)
        .await
    {
        Ok(q) => ValidationCheck {
            name,
            ok: true,
            detail: format!(
                "quote: {} → {} out={} impact={:.4}%",
                q.token_in, q.token_out, q.amount_out_wei, q.price_impact
            ),
        },
        Err(e) => ValidationCheck {
            name,
            ok: false,
            detail: format!("quote fetch failed: {e}"),
        },
    }
}

async fn check_wallet_balance(
    client: &reqwest::Client,
    keymaster_url: &str,
    keymaster_token: &str,
    chain_registry: &ChainRegistry,
) -> ValidationCheck {
    let name = "wallet_has_balance".to_string();

    //best-effort: ask keymaster to refresh its cached balances first.
    //we do not block on this — the actual check queries the rpc directly below.
    let _ = client
        .get(format!("{}/balances/refresh", keymaster_url))
        .header("Authorization", format!("Bearer {keymaster_token}"))
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    let resp = match client
        .get(format!("{}/wallets", keymaster_url))
        .header("Authorization", format!("Bearer {keymaster_token}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return ValidationCheck {
                name,
                ok: false,
                detail: format!("Keymaster wallets unreachable: {e}"),
            };
        }
    };

    let data: serde_json::Value = resp.json().await.unwrap_or_default();
    let wallets = data.as_array().or_else(|| data["wallets"].as_array());
    let wallets = match wallets {
        Some(ws) => ws,
        None => {
            return ValidationCheck {
                name,
                ok: false,
                detail: "Could not parse wallet list from Keymaster".into(),
            };
        }
    };

    let total = wallets.len();

    //pick a fallback rpc url for wallets whose chain isn't directly configured
    //(e.g. wallet.chain = "base_sepolia" but only `base` has an rpc url set in env).
    let fallback_rpc = chain_registry
        .available()
        .into_iter()
        .next()
        .map(|c| c.rpc_url.clone());

    for w in wallets {
        let address = match w["address"].as_str() {
            Some(a) if !a.is_empty() => a,
            _ => continue,
        };

        let wallet_chain = w["chain"].as_str().unwrap_or("");
        let rpc_url = chain_registry
            .get(wallet_chain)
            .map(|c| c.rpc_url.clone())
            .filter(|u| !u.is_empty())
            .or_else(|| fallback_rpc.clone());

        let rpc_url = match rpc_url {
            Some(u) => u,
            None => continue,
        };

        match fetch_eth_balance(client, &rpc_url, address).await {
            Ok(wei) if wei > 0 => {
                return ValidationCheck {
                    name,
                    ok: true,
                    detail: format!(
                        "{} wallets, {} has balance {} wei (chain={})",
                        total, address, wei, wallet_chain
                    ),
                };
            }
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!(
                    "[validate] eth_getBalance failed for {} on {}: {}",
                    address,
                    wallet_chain,
                    e
                );
                continue;
            }
        }
    }

    ValidationCheck {
        name,
        ok: false,
        detail: format!("{} wallets, none have balance > 0 (queried RPC directly)", total),
    }
}

async fn fetch_eth_balance(
    client: &reqwest::Client,
    rpc_url: &str,
    address: &str,
) -> anyhow::Result<u128> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_getBalance",
        "params": [address, "latest"],
        "id": 1
    });

    let resp = client
        .post(rpc_url)
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await?;

    let data: serde_json::Value = resp.json().await?;
    if let Some(err) = data.get("error") {
        anyhow::bail!("rpc error: {}", err);
    }
    let hex = data["result"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing result"))?;
    let wei = u128::from_str_radix(hex.trim_start_matches("0x"), 16)?;
    Ok(wei)
}

async fn check_gas_estimate(
    client: &reqwest::Client,
    chain_registry: &ChainRegistry,
) -> ValidationCheck {
    let name = "gas_estimate_succeeds".to_string();

    let chain = chain_registry.available().into_iter().next();
    let chain = match chain {
        Some(c) => c,
        None => {
            return ValidationCheck {
                name,
                ok: false,
                detail: "No chains available".into(),
            }
        }
    };

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "eth_estimateGas",
        "params": [{
            "from": "0x0000000000000000000000000000000000000001",
            "to": "0x0000000000000000000000000000000000000002",
            "value": "0x0"
        }],
        "id": 1
    });

    match client.post(&chain.rpc_url).json(&body).send().await {
        Ok(r) => {
            let data: serde_json::Value = r.json().await.unwrap_or_default();
            if let Some(gas_hex) = data["result"].as_str() {
                let gas =
                    u64::from_str_radix(gas_hex.trim_start_matches("0x"), 16).unwrap_or(0);
                ValidationCheck {
                    name,
                    ok: gas > 0,
                    detail: format!("{}: estimated gas = {}", chain.name, gas),
                }
            } else {
                let err = data["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error");
                ValidationCheck {
                    name,
                    ok: false,
                    detail: format!("{}: gas estimate error: {}", chain.name, err),
                }
            }
        }
        Err(e) => ValidationCheck {
            name,
            ok: false,
            detail: format!("gas estimate RPC failed: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_parsing() {
        assert_eq!(
            u64::from_str_radix("5208".trim_start_matches("0x"), 16).unwrap(),
            21000
        );
        assert_eq!(
            u64::from_str_radix("0x5208".trim_start_matches("0x"), 16).unwrap(),
            21000
        );
    }
}
