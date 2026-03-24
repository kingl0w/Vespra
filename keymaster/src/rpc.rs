use alloy::network::EthereumWallet;
use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::rpc::types::TransactionRequest;
use std::str::FromStr;

use crate::config::ChainConfig;
use crate::error::{AppError, AppResult};

pub async fn get_balance(chain: &ChainConfig, address: &str) -> AppResult<U256> {
    let addr = Address::from_str(address)
        .map_err(|e| AppError::BadRequest(format!("Invalid address: {e}")))?;
    let provider = ProviderBuilder::new()
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);
    provider.get_balance(addr).await
        .map_err(|e| AppError::Rpc(format!("Balance query failed: {e}")))
}

pub async fn send_native(
    chain: &ChainConfig, private_key_bytes: &[u8], to: &str, value: U256,
) -> AppResult<String> {
    let signer = PrivateKeySigner::from_slice(private_key_bytes)
        .map_err(|e| AppError::Transaction(format!("Invalid private key: {e}")))?;
    let wallet = EthereumWallet::from(signer);
    let to_addr = Address::from_str(to)
        .map_err(|e| AppError::BadRequest(format!("Invalid to address: {e}")))?;
    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);
    let tx = TransactionRequest::default().to(to_addr).value(value);
    let pending = provider.send_transaction(tx).await
        .map_err(|e| AppError::Transaction(format!("Send failed: {e}")))?;
    Ok(format!("{:?}", pending.tx_hash()))
}

pub async fn send_transaction(
    chain: &ChainConfig, private_key_bytes: &[u8], tx_request: TransactionRequest,
) -> AppResult<String> {
    let signer = PrivateKeySigner::from_slice(private_key_bytes)
        .map_err(|e| AppError::Transaction(format!("Invalid private key: {e}")))?;
    let wallet = EthereumWallet::from(signer);
    let provider = ProviderBuilder::new()
        .with_recommended_fillers()
        .wallet(wallet)
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);
    let pending = provider.send_transaction(tx_request).await
        .map_err(|e| AppError::Transaction(format!("Transaction failed: {e}")))?;
    Ok(format!("{:?}", pending.tx_hash()))
}

pub async fn estimate_gas(chain: &ChainConfig, tx_request: TransactionRequest) -> AppResult<u64> {
    let provider = ProviderBuilder::new()
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);
    provider.estimate_gas(&tx_request).await
        .map_err(|e| AppError::Rpc(format!("Gas estimation failed: {e}")))
}

pub async fn get_gas_price(chain: &ChainConfig) -> AppResult<u128> {
    let provider = ProviderBuilder::new()
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);
    provider.get_gas_price().await
        .map_err(|e| AppError::Rpc(format!("Gas price query failed: {e}")))
}

pub async fn get_block_number(chain: &ChainConfig) -> AppResult<u64> {
    let provider = ProviderBuilder::new()
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);
    provider.get_block_number().await
        .map_err(|e| AppError::Rpc(format!("Block number query failed: {e}")))
}

pub async fn simulate_tx(
    chain: &ChainConfig,
    from: &str,
    to: &str,
    value: U256,
) -> AppResult<()> {
    let from_addr = Address::from_str(from)
        .map_err(|e| AppError::BadRequest(format!("Invalid from address: {e}")))?;
    let to_addr = Address::from_str(to)
        .map_err(|e| AppError::BadRequest(format!("Invalid to address: {e}")))?;

    let provider = ProviderBuilder::new()
        .on_http(chain.rpc_url.parse().map_err(|e| AppError::Rpc(format!("Invalid RPC URL: {e}")))?);

    let tx = TransactionRequest::default()
        .from(from_addr)
        .to(to_addr)
        .value(value);

    provider.call(&tx).await
        .map(|_| ())
        .map_err(|e| AppError::Transaction(format!("Simulation reverted: {e}")))
}
