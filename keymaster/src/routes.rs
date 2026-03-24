use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::keystore::{WalletInfo, WalletRecord};
use crate::rpc;
use crate::state::AppState;

// ─── Health ──────────────────────────────────────────────────────

pub async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    let chains = state.config.active_chains();
    let chain_names: Vec<&String> = chains.iter().map(|(name, _)| *name).collect();
    Json(json!({
        "status": "ok",
        "service": "vespra-keymaster",
        "version": env!("CARGO_PKG_VERSION"),
        "chains": chain_names,
    }))
}

// ─── Wallet CRUD ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateWalletRequest {
    pub chain: String,
    pub label: Option<String>,
    pub cap_eth: Option<String>,
    pub strategy: Option<String>,
}

pub async fn create_wallet(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateWalletRequest>,
) -> AppResult<(StatusCode, Json<WalletInfo>)> {
    state.config.get_chain(&req.chain)
        .ok_or_else(|| AppError::ChainNotConfigured(req.chain.clone()))?;

    let signer = PrivateKeySigner::random();
    let address = format!("{:?}", signer.address());
    let private_key_bytes = signer.to_bytes();
    let encrypted = crypto::encrypt_key(private_key_bytes.as_slice(), &state.master_password)?;

    let cap_wei = if let Some(cap_eth) = &req.cap_eth {
        eth_to_wei(cap_eth)?
    } else {
        "0".to_string()
    };

    let now = Utc::now().to_rfc3339();
    let id = uuid::Uuid::new_v4().to_string();
    let record = WalletRecord {
        id: id.clone(), address: address.clone(), chain: req.chain.clone(),
        label: req.label.unwrap_or_default(), encrypted_key: encrypted,
        cap_wei, strategy: req.strategy.unwrap_or_default(),
        active: true, created_at: now.clone(), updated_at: now,
    };
    state.keystore.insert_wallet(&record)?;

    tracing::info!(wallet_id = %id, address = %address, chain = %req.chain, "Created new burner wallet");
    Ok((StatusCode::CREATED, Json(WalletInfo::from(record))))
}

#[derive(Debug, Deserialize)]
pub struct ListWalletsQuery {
    pub chain: Option<String>,
    pub strategy: Option<String>,
}

pub async fn list_wallets(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListWalletsQuery>,
) -> AppResult<Json<Vec<WalletInfo>>> {
    let wallets = state.keystore.list_wallets(q.chain.as_deref(), q.strategy.as_deref())?;
    Ok(Json(wallets))
}

pub async fn get_wallet(
    State(state): State<Arc<AppState>>,
    Path(wallet_id): Path<String>,
) -> AppResult<Json<WalletInfo>> {
    let record = state.keystore.get_wallet(&wallet_id)?;
    Ok(Json(WalletInfo::from(record)))
}

pub async fn deactivate_wallet(
    State(state): State<Arc<AppState>>,
    Path(wallet_id): Path<String>,
) -> AppResult<Json<Value>> {
    state.keystore.deactivate_wallet(&wallet_id)?;
    tracing::info!(wallet_id = %wallet_id, "Deactivated wallet");
    Ok(Json(json!({ "status": "deactivated", "wallet_id": wallet_id })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateCapRequest {
    pub cap_eth: String,
}

pub async fn update_cap(
    State(state): State<Arc<AppState>>,
    Path(wallet_id): Path<String>,
    Json(req): Json<UpdateCapRequest>,
) -> AppResult<Json<Value>> {
    let cap_wei = eth_to_wei(&req.cap_eth)?;
    state.keystore.update_cap(&wallet_id, &cap_wei)?;
    tracing::info!(wallet_id = %wallet_id, cap_eth = %req.cap_eth, "Updated wallet cap");
    Ok(Json(json!({ "status": "updated", "wallet_id": wallet_id, "cap_wei": cap_wei })))
}

// ─── Balance & Chain Queries ─────────────────────────────────────

pub async fn get_balance(
    State(state): State<Arc<AppState>>,
    Path((chain_name, address)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    let chain = state.config.get_chain(&chain_name)
        .ok_or_else(|| AppError::ChainNotConfigured(chain_name.clone()))?;
    let balance = rpc::get_balance(chain, &address).await?;
    let balance_eth = wei_to_eth_string(balance);
    Ok(Json(json!({
        "address": address, "chain": chain_name,
        "balance_wei": balance.to_string(), "balance_eth": balance_eth,
    })))
}

pub async fn get_all_balances(
    State(state): State<Arc<AppState>>,
    Path(chain_name): Path<String>,
) -> AppResult<Json<Value>> {
    let chain = state.config.get_chain(&chain_name)
        .ok_or_else(|| AppError::ChainNotConfigured(chain_name.clone()))?;
    let wallets = state.keystore.list_wallets(Some(&chain_name), None)?;
    let mut results = Vec::new();
    for w in &wallets {
        if !w.active { continue; }
        match rpc::get_balance(chain, &w.address).await {
            Ok(balance) => {
                results.push(json!({
                    "id": w.id, "address": w.address, "label": w.label,
                    "strategy": w.strategy, "balance_wei": balance.to_string(),
                    "balance_eth": wei_to_eth_string(balance), "cap_wei": w.cap_wei,
                }));
            }
            Err(e) => {
                results.push(json!({ "id": w.id, "address": w.address, "error": e.to_string() }));
            }
        }
    }
    Ok(Json(json!({ "chain": chain_name, "wallets": results })))
}

pub async fn chain_status(
    State(state): State<Arc<AppState>>,
    Path(chain_name): Path<String>,
) -> AppResult<Json<Value>> {
    let chain = state.config.get_chain(&chain_name)
        .ok_or_else(|| AppError::ChainNotConfigured(chain_name.clone()))?;
    let block = rpc::get_block_number(chain).await?;
    let gas_price = rpc::get_gas_price(chain).await?;
    Ok(Json(json!({
        "chain": chain_name, "chain_id": chain.chain_id,
        "block_number": block, "gas_price_gwei": gas_price as f64 / 1e9,
    })))
}

// ─── Transactions ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SendNativeRequest {
    pub wallet_id: String,
    pub to: String,
    pub amount_eth: String,
}

pub async fn send_native(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SendNativeRequest>,
) -> AppResult<Json<Value>> {
    let wallet = state.keystore.get_wallet(&req.wallet_id)?;
    if !wallet.active {
        return Err(AppError::BadRequest("Wallet is deactivated".into()));
    }

    // ── Address validation ───────────────────────────────────────
    let to_normalized = req.to.trim().to_lowercase();
    let zero_address = "0x0000000000000000000000000000000000000000";
    if to_normalized == zero_address {
        return Err(AppError::BadRequest("Cannot send to the zero address".into()));
    }
    if to_normalized == wallet.address.to_lowercase() {
        return Err(AppError::BadRequest("Cannot send to the wallet's own address (self-send)".into()));
    }

    let chain = state.config.get_chain(&wallet.chain)
        .ok_or_else(|| AppError::ChainNotConfigured(wallet.chain.clone()))?;

    let value = eth_to_u256(&req.amount_eth)?;

    // ── Wallet cap enforcement ───────────────────────────────────
    let cap_wei_str = &wallet.cap_wei;
    if cap_wei_str != "0" && !cap_wei_str.is_empty() {
        let cap = cap_wei_str.parse::<u128>()
            .map_err(|_| AppError::Internal(format!("Invalid cap_wei in DB: {cap_wei_str}")))?;
        let cap_u256 = U256::from(cap);
        let total_sent = state.keystore.total_sent_wei(&req.wallet_id)?;
        let remaining = if cap_u256 > total_sent { cap_u256 - total_sent } else { U256::ZERO };
        if value > remaining {
            return Err(AppError::CapExceeded {
                balance: value.to_string(),
                cap: remaining.to_string(),
            });
        }
    }

    // ── TX simulation (eth_call before broadcast) ────────────────
    let sim_result = rpc::simulate_tx(chain, &wallet.address, &req.to, value).await;
    let (simulated, simulation_result, revert_reason) = match sim_result {
        Ok(()) => (true, "success".to_string(), None::<String>),
        Err(e) => {
            let reason = e.to_string();
            tracing::warn!(
                wallet_id = %req.wallet_id,
                to = %req.to,
                reason = %reason,
                "TX simulation reverted — aborting broadcast"
            );
            state.keystore.log_tx(
                &req.wallet_id, &wallet.chain, None,
                "send_native", &req.to, &req.amount_eth,
                "simulation_failed", Some(&reason),
            )?;
            return Ok(Json(json!({
                "status": "simulation_failed",
                "simulated": true,
                "simulation_result": "revert",
                "revert_reason": reason,
                "attempts": 0,
            })));
        }
    };

    // ── Broadcast with exponential backoff retry (3 attempts) ────
    let mut pk_bytes = crypto::decrypt_key(&wallet.encrypted_key, &state.master_password)?;
    let max_attempts = 3u32;
    let mut last_error = String::new();
    let mut attempts = 0u32;

    let mut result = Ok(String::new());
    for attempt in 1..=max_attempts {
        attempts = attempt;
        result = rpc::send_native(chain, &pk_bytes, &req.to, value).await;
        match &result {
            Ok(_) => break,
            Err(e) => {
                last_error = e.to_string();
                let is_transient = last_error.contains("timeout")
                    || last_error.contains("503")
                    || last_error.contains("rate limit")
                    || last_error.contains("connection");
                tracing::warn!(
                    wallet_id = %req.wallet_id,
                    attempt = attempt,
                    error = %last_error,
                    "send_native attempt failed"
                );
                if !is_transient || attempt == max_attempts {
                    break;
                }
                // Exponential backoff: 1s, 2s (attempt 1→2, 2→3)
                let delay_ms = 1000u64 * 2u64.pow(attempt - 1);
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
    crypto::zeroize_bytes(&mut pk_bytes);

    match result {
        Ok(tx_hash) => {
            state.keystore.log_tx(
                &req.wallet_id, &wallet.chain, Some(&tx_hash),
                "send_native", &req.to, &req.amount_eth, "confirmed", None,
            )?;
            tracing::info!(wallet_id = %req.wallet_id, tx_hash = %tx_hash, attempts = %attempts, "Sent native token");
            Ok(Json(json!({
                "status": "ok",
                "tx_hash": tx_hash,
                "from": wallet.address,
                "to": req.to,
                "amount_eth": req.amount_eth,
                "chain": wallet.chain,
                "simulated": simulated,
                "simulation_result": simulation_result,
                "revert_reason": revert_reason,
                "attempts": attempts,
            })))
        }
        Err(e) => {
            state.keystore.log_tx(
                &req.wallet_id, &wallet.chain, None,
                "send_native", &req.to, &req.amount_eth, "failed", Some(&e.to_string()),
            )?;
            Err(e)
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SweepRequest {
    pub wallet_id: String,
}

pub async fn sweep_to_safe(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SweepRequest>,
) -> AppResult<Json<Value>> {
    let wallet = state.keystore.get_wallet(&req.wallet_id)?;
    let chain = state.config.get_chain(&wallet.chain)
        .ok_or_else(|| AppError::ChainNotConfigured(wallet.chain.clone()))?;
    // Check DB first, fall back to .env config
    let db_safe = state.keystore.get_setting(&format!("safe_{}", wallet.chain))?;
    let safe_address = db_safe.as_deref()
        .or(chain.safe_address.as_deref())
        .ok_or_else(|| AppError::BadRequest(format!("No Safe configured for chain {}", wallet.chain)))?
        .to_string();

    let balance = rpc::get_balance(chain, &wallet.address).await?;
    if balance.is_zero() {
        return Ok(Json(json!({ "status": "skip", "reason": "Zero balance", "wallet_id": req.wallet_id })));
    }

    let sweep_amount = balance * U256::from(95) / U256::from(100);
    if sweep_amount.is_zero() {
        return Ok(Json(json!({
            "status": "skip", "reason": "Balance too low to sweep",
            "balance_wei": balance.to_string(),
        })));
    }
    let mut pk_bytes = crypto::decrypt_key(&wallet.encrypted_key, &state.master_password)?;
    let result = rpc::send_native(chain, &pk_bytes, &safe_address, sweep_amount).await;
    crypto::zeroize_bytes(&mut pk_bytes);

    match result {
        Ok(tx_hash) => {
            state.keystore.log_tx(&req.wallet_id, &wallet.chain, Some(&tx_hash),
                "sweep_to_safe", &safe_address, &sweep_amount.to_string(), "confirmed", None)?;
            tracing::info!(wallet_id = %req.wallet_id, tx_hash = %tx_hash, "Swept to Safe");
            Ok(Json(json!({
                "status": "ok", "tx_hash": tx_hash, "from": wallet.address,
                "to": safe_address, "amount_wei": sweep_amount.to_string(),
                "amount_eth": wei_to_eth_string(sweep_amount), "chain": wallet.chain,
            })))
        }
        Err(e) => {
            state.keystore.log_tx(&req.wallet_id, &wallet.chain, None,
                "sweep_to_safe", &safe_address, &sweep_amount.to_string(), "failed", Some(&e.to_string()))?;
            Err(e)
        }
    }
}

pub async fn get_tx_log(
    State(state): State<Arc<AppState>>,
    Path(wallet_id): Path<String>,
) -> AppResult<Json<Value>> {
    let txs = state.keystore.get_tx_log(&wallet_id, 50)?;
    Ok(Json(json!({ "wallet_id": wallet_id, "transactions": txs })))
}

// ─── Settings ────────────────────────────────────────────────────

pub async fn get_safes(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<Value>> {
    let settings = state.keystore.list_settings_by_prefix("safe_")?;
    let mut safes = serde_json::Map::new();
    for (key, value) in settings {
        let chain = key.strip_prefix("safe_").unwrap_or(&key);
        safes.insert(chain.to_string(), Value::String(value));
    }
    Ok(Json(Value::Object(safes)))
}

#[derive(Debug, Deserialize)]
pub struct SetSafeRequest {
    pub address: String,
}

pub async fn set_safe(
    State(state): State<Arc<AppState>>,
    Path(chain): Path<String>,
    Json(req): Json<SetSafeRequest>,
) -> AppResult<Json<Value>> {
    // Validate chain exists
    state.config.get_chain(&chain)
        .ok_or_else(|| AppError::ChainNotConfigured(chain.clone()))?;

    // Basic address validation
    if !req.address.starts_with("0x") || req.address.len() != 42 {
        return Err(AppError::BadRequest("Invalid address format".into()));
    }

    let key = format!("safe_{chain}");
    state.keystore.set_setting(&key, &req.address)?;
    tracing::info!(chain = %chain, address = %req.address, "Updated Safe address");
    Ok(Json(json!({ "status": "ok", "chain": chain, "address": req.address })))
}

// ─── NullBoiler Dispatch ─────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct DispatchRequest {
    pub task_id: String,
    pub prompt: Option<String>,
    pub input: Option<Value>,
}

pub async fn dispatch(
    State(state): State<Arc<AppState>>,
    Json(req): Json<DispatchRequest>,
) -> Json<Value> {
    let prompt = req.prompt.as_deref().unwrap_or("");
    let input = req.input.unwrap_or(Value::Object(serde_json::Map::new()));
    let task_id = &req.task_id;

    let action = resolve_action(prompt, &input);
    tracing::info!(task_id = %task_id, action = %action, "NullBoiler dispatch");

    let result = match action.as_str() {
        "create_wallet" => dispatch_create_wallet(state, &input).await,
        "list_wallets" => dispatch_list_wallets(state, &input).await,
        "get_wallet" => dispatch_get_wallet(state, &input).await,
        "deactivate_wallet" => dispatch_deactivate_wallet(state, &input).await,
        "update_cap" => dispatch_update_cap(state, &input).await,
        "get_balance" => dispatch_get_balance(state, &input).await,
        "get_all_balances" => dispatch_get_all_balances(state, &input).await,
        "chain_status" => dispatch_chain_status(state, &input).await,
        "send_native" => dispatch_send_native(state, &input).await,
        "sweep" => dispatch_sweep(state, &input).await,
        "get_tx_log" => dispatch_get_tx_log(state, &input).await,
        "health" => Ok(json!({
            "status": "ok",
            "service": "vespra-keymaster",
            "version": env!("CARGO_PKG_VERSION"),
        })),
        _ => Err(format!("Unknown action: '{action}'. Available: create_wallet, list_wallets, get_wallet, \
            deactivate_wallet, update_cap, get_balance, get_all_balances, chain_status, send_native, sweep, get_tx_log, health")),
    };

    match result {
        Ok(data) => Json(json!({ "status": "ok", "task_id": task_id, "response": data })),
        Err(e) => Json(json!({ "status": "error", "task_id": task_id, "response": e })),
    }
}

fn resolve_action(prompt: &str, input: &Value) -> String {
    // Explicit action field takes priority
    if let Some(action) = input.get("action").and_then(|v| v.as_str()) {
        return action.to_string();
    }

    let p = prompt.to_lowercase();

    if p.contains("create") && p.contains("wallet") { return "create_wallet".into(); }
    if p.contains("list") && p.contains("wallet") { return "list_wallets".into(); }
    if p.contains("deactivat") && p.contains("wallet") { return "deactivate_wallet".into(); }
    if p.contains("cap") && (p.contains("update") || p.contains("set")) { return "update_cap".into(); }
    if p.contains("sweep") { return "sweep".into(); }
    if p.contains("send") && (p.contains("tx") || p.contains("transaction") || p.contains("native") || p.contains("eth")) { return "send_native".into(); }
    if p.contains("all") && p.contains("balance") { return "get_all_balances".into(); }
    if p.contains("balance") { return "get_balance".into(); }
    if p.contains("chain") && p.contains("status") { return "chain_status".into(); }
    if p.contains("tx") && p.contains("log") { return "get_tx_log".into(); }
    if (p.contains("get") || p.contains("fetch") || p.contains("show")) && p.contains("wallet") { return "get_wallet".into(); }
    if p.contains("health") || p.contains("ping") { return "health".into(); }

    "unknown".into()
}

fn str_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(|v| v.as_str())
}

fn str_or_num_field(input: &Value, key: &str) -> Option<String> {
    match input.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(v) if v.is_number() => Some(v.to_string()),
        _ => None,
    }
}

async fn dispatch_create_wallet(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let chain = str_field(input, "chain").unwrap_or("sepolia");
    let req = CreateWalletRequest {
        chain: chain.to_string(),
        label: str_field(input, "label").map(String::from),
        cap_eth: str_or_num_field(input, "cap_eth"),
        strategy: str_field(input, "strategy").map(String::from),
    };
    let (_, Json(info)) = create_wallet(State(state), Json(req)).await.map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

async fn dispatch_list_wallets(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let q = ListWalletsQuery {
        chain: str_field(input, "chain").map(String::from),
        strategy: str_field(input, "strategy").map(String::from),
    };
    let Json(wallets) = list_wallets(State(state), Query(q)).await.map_err(|e| e.to_string())?;
    serde_json::to_value(wallets).map_err(|e| e.to_string())
}

async fn dispatch_get_wallet(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let wallet_id = str_field(input, "wallet_id").ok_or("missing wallet_id")?;
    let Json(info) = get_wallet(State(state), Path(wallet_id.to_string())).await.map_err(|e| e.to_string())?;
    serde_json::to_value(info).map_err(|e| e.to_string())
}

async fn dispatch_deactivate_wallet(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let wallet_id = str_field(input, "wallet_id").ok_or("missing wallet_id")?;
    let Json(v) = deactivate_wallet(State(state), Path(wallet_id.to_string())).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_update_cap(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let wallet_id = str_field(input, "wallet_id").ok_or("missing wallet_id")?;
    let cap_eth = str_or_num_field(input, "cap_eth").ok_or("missing cap_eth")?;
    let req = UpdateCapRequest { cap_eth: cap_eth.to_string() };
    let Json(v) = update_cap(State(state), Path(wallet_id.to_string()), Json(req)).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_get_balance(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let chain = str_field(input, "chain").ok_or("missing chain")?;
    let address = str_field(input, "address").ok_or("missing address")?;
    let Json(v) = get_balance(State(state), Path((chain.to_string(), address.to_string()))).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_get_all_balances(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let chain = str_field(input, "chain").ok_or("missing chain")?;
    let Json(v) = get_all_balances(State(state), Path(chain.to_string())).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_chain_status(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let chain = str_field(input, "chain").ok_or("missing chain")?;
    let Json(v) = chain_status(State(state), Path(chain.to_string())).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_send_native(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let req = SendNativeRequest {
        wallet_id: str_field(input, "wallet_id").ok_or("missing wallet_id")?.to_string(),
        to: str_field(input, "to").ok_or("missing to")?.to_string(),
        amount_eth: str_or_num_field(input, "amount_eth").ok_or("missing amount_eth")?,
    };
    let Json(v) = send_native(State(state), Json(req)).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_sweep(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let wallet_id = str_field(input, "wallet_id").ok_or("missing wallet_id")?;
    let req = SweepRequest { wallet_id: wallet_id.to_string() };
    let Json(v) = sweep_to_safe(State(state), Json(req)).await.map_err(|e| e.to_string())?;
    Ok(v)
}

async fn dispatch_get_tx_log(state: Arc<AppState>, input: &Value) -> Result<Value, String> {
    let wallet_id = str_field(input, "wallet_id").ok_or("missing wallet_id")?;
    let Json(v) = get_tx_log(State(state), Path(wallet_id.to_string())).await.map_err(|e| e.to_string())?;
    Ok(v)
}

// ─── Helpers ─────────────────────────────────────────────────────

fn eth_to_wei(eth: &str) -> AppResult<String> {
    let value = eth_to_u256(eth)?;
    Ok(value.to_string())
}

fn eth_to_u256(eth: &str) -> AppResult<U256> {
    let parts: Vec<&str> = eth.split('.').collect();
    match parts.len() {
        1 => {
            let whole = parts[0].parse::<u128>()
                .map_err(|_| AppError::BadRequest(format!("Invalid ETH amount: {eth}")))?;
            Ok(U256::from(whole) * U256::from(10u128.pow(18)))
        }
        2 => {
            let whole = if parts[0].is_empty() { 0u128 } else {
                parts[0].parse::<u128>()
                    .map_err(|_| AppError::BadRequest(format!("Invalid ETH amount: {eth}")))?
            };
            let decimal_str = parts[1];
            let decimal_len = decimal_str.len().min(18);
            let decimal_padded = format!("{:0<18}", &decimal_str[..decimal_len]);
            let decimal = decimal_padded.parse::<u128>()
                .map_err(|_| AppError::BadRequest(format!("Invalid ETH amount: {eth}")))?;
            Ok(U256::from(whole) * U256::from(10u128.pow(18)) + U256::from(decimal))
        }
        _ => Err(AppError::BadRequest(format!("Invalid ETH amount: {eth}"))),
    }
}

fn wei_to_eth_string(wei: U256) -> String {
    let divisor = U256::from(10u128.pow(18));
    let whole = wei / divisor;
    let remainder = wei % divisor;
    if remainder.is_zero() {
        format!("{whole}.0")
    } else {
        let rem_str = format!("{:0>18}", remainder);
        let trimmed = rem_str.trim_end_matches('0');
        format!("{whole}.{trimmed}")
    }
}
