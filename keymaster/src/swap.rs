
use alloy::primitives::{Address, U256};
use alloy::sol;
use alloy::sol_types::SolCall;
use std::str::FromStr;

use crate::config::ChainConfig;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::keystore::WalletRecord;
use crate::rpc;

//─── solidity bindings ─────────────────────────────────────────────

sol! {
    interface IWETH9 {
        function deposit() external payable;
    }

    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function allowance(address owner, address spender) external view returns (uint256);
        function balanceOf(address owner) external view returns (uint256);
    }

    interface ISwapRouter02 {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        function exactInputSingle(ExactInputSingleParams calldata params)
            external payable returns (uint256 amountOut);
    }
}

//─── per-chain swap config ─────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct ChainSwapConfig {
    pub router: Address,
    pub weth: Address,
}

///hardcoded router/weth addresses per chain. returns `none` for chains
///where the swap path isn't wired up yet.
pub fn swap_config(chain_name: &str) -> Option<ChainSwapConfig> {
    //both base and base sepolia use the same canonical weth9 address.
    let weth = Address::from_str("0x4200000000000000000000000000000000000006").ok()?;
    match chain_name {
        "base_sepolia" => Some(ChainSwapConfig {
            router: Address::from_str("0x94cC0AaC535CCDB3C01d6787D6413C739ae12bc4").ok()?,
            weth,
        }),
        "base" => Some(ChainSwapConfig {
            router: Address::from_str("0x2626664c2603336E57B271c5C0b26F421741e481").ok()?,
            weth,
        }),
        _ => None,
    }
}

//─── abi encode helpers ────────────────────────────────────────────

pub fn encode_weth_deposit() -> Vec<u8> {
    IWETH9::depositCall {}.abi_encode()
}

pub fn encode_erc20_approve(spender: Address, amount: U256) -> Vec<u8> {
    IERC20::approveCall { spender, amount }.abi_encode()
}

pub fn encode_exact_input_single(
    token_in: Address,
    token_out: Address,
    recipient: Address,
    amount_in: U256,
) -> Vec<u8> {
    let params = ISwapRouter02::ExactInputSingleParams {
        tokenIn: token_in,
        tokenOut: token_out,
        fee: alloy::primitives::Uint::<24, 1>::from(3000u32),
        recipient,
        amountIn: amount_in,
        amountOutMinimum: U256::ZERO,
        sqrtPriceLimitX96: alloy::primitives::U160::ZERO,
    };
    ISwapRouter02::exactInputSingleCall { params }.abi_encode()
}

//─── erc-20 read helpers ───────────────────────────────────────────

pub async fn read_balance(
    chain: &ChainConfig,
    token: Address,
    owner: Address,
) -> AppResult<U256> {
    let data = IERC20::balanceOfCall { owner }.abi_encode();
    let raw = rpc::eth_call(chain, token, data).await?;
    let decoded = IERC20::balanceOfCall::abi_decode_returns(&raw, true)
        .map_err(|e| AppError::Rpc(format!("balanceOf decode failed: {e}")))?;
    Ok(decoded._0)
}

pub async fn read_allowance(
    chain: &ChainConfig,
    token: Address,
    owner: Address,
    spender: Address,
) -> AppResult<U256> {
    let data = IERC20::allowanceCall { owner, spender }.abi_encode();
    let raw = rpc::eth_call(chain, token, data).await?;
    let decoded = IERC20::allowanceCall::abi_decode_returns(&raw, true)
        .map_err(|e| AppError::Rpc(format!("allowance decode failed: {e}")))?;
    Ok(decoded._0)
}

//─── swap orchestration ────────────────────────────────────────────

///outcome of a successful `/swap` request. hashes are populated only for the
///steps that actually ran (wrap and approve are skipped when not needed).
pub struct SwapResult {
    pub swap_tx_hash: String,
    pub wrap_tx_hash: Option<String>,
    pub approve_tx_hash: Option<String>,
}

pub async fn execute_swap(
    chain_name: &str,
    chain: &ChainConfig,
    wallet: &WalletRecord,
    master_password: &str,
    token_in_input: &str,
    token_out: &str,
    amount_in_wei: U256,
) -> AppResult<SwapResult> {
    let cfg = swap_config(chain_name).ok_or_else(|| {
        AppError::BadRequest(format!("Swap not configured for chain '{chain_name}'"))
    })?;

    //resolve token_in: "eth" → weth address. caller is allowed to pass either
    //the literal string "eth" or the weth address directly.
    let token_in_lower = token_in_input.trim().to_lowercase();
    let is_eth_input = token_in_lower == "eth";
    let token_in: Address = if is_eth_input {
        cfg.weth
    } else {
        Address::from_str(token_in_input)
            .map_err(|e| AppError::BadRequest(format!("Invalid token_in address: {e}")))?
    };
    let token_out_addr = Address::from_str(token_out)
        .map_err(|e| AppError::BadRequest(format!("Invalid token_out address: {e}")))?;
    let wallet_addr = Address::from_str(&wallet.address)
        .map_err(|e| AppError::BadRequest(format!("Invalid wallet address: {e}")))?;

    if token_in == token_out_addr {
        return Err(AppError::BadRequest(
            "token_in and token_out must differ".into(),
        ));
    }

    //decrypt the private key once and reuse it across all sub-transactions.
    let mut pk_bytes = crypto::decrypt_key(&wallet.encrypted_key, master_password)?;
    let result = execute_swap_inner(
        chain,
        &pk_bytes,
        cfg,
        wallet_addr,
        token_in,
        token_out_addr,
        amount_in_wei,
        is_eth_input || token_in == cfg.weth,
    )
    .await;
    crypto::zeroize_bytes(&mut pk_bytes);
    result
}

#[allow(clippy::too_many_arguments)]
async fn execute_swap_inner(
    chain: &ChainConfig,
    pk_bytes: &[u8],
    cfg: ChainSwapConfig,
    wallet_addr: Address,
    token_in: Address,
    token_out: Address,
    amount_in_wei: U256,
    token_in_is_weth: bool,
) -> AppResult<SwapResult> {
    let mut wrap_tx_hash: Option<String> = None;
    let mut approve_tx_hash: Option<String> = None;

    if token_in_is_weth {
        let weth_balance = read_balance(chain, cfg.weth, wallet_addr).await?;
        if weth_balance >= amount_in_wei {
            tracing::info!(
                wallet = %format!("{wallet_addr:?}"),
                weth_balance = %weth_balance,
                "[swap] WETH balance sufficient ({}) — skipping wrap step",
                weth_balance
            );
        } else {
            let needed = amount_in_wei - weth_balance;
            tracing::info!(
                wallet = %wallet_addr.to_checksum(None),
                weth_balance = %weth_balance,
                wrap_amount = %needed,
                "[swap] wrapping ETH → WETH"
            );
            let data = encode_weth_deposit();
            match rpc::send_tx_and_wait(
                chain,
                pk_bytes,
                Some(&format!("{:?}", cfg.weth)),
                needed,
                Some(data),
            )
            .await
            {
                Ok(hash) => {
                    tracing::info!(tx_hash = %hash, "[swap] wrap confirmed");
                    wrap_tx_hash = Some(hash);
                }
                Err(e) => {
                    //re-check balance — a concurrent top-up may have made
                    //the wrap unnecessary even though our attempt errored.
                    let post_failure_balance =
                        read_balance(chain, cfg.weth, wallet_addr).await.unwrap_or(weth_balance);
                    if post_failure_balance >= amount_in_wei {
                        tracing::warn!(
                            wallet = %format!("{wallet_addr:?}"),
                            weth_balance = %post_failure_balance,
                            error = %e,
                            "[swap] wrap failed but WETH balance now sufficient — proceeding"
                        );
                    } else {
                        return Err(AppError::Transaction(format!(
                            "wrap step failed: {e}"
                        )));
                    }
                }
            }
        }
    }

    //── step 2: approve router for token_in if allowance is too low ──
    let current_allowance = read_allowance(chain, token_in, wallet_addr, cfg.router).await?;
    if current_allowance < amount_in_wei {
        tracing::info!(
            wallet = %wallet_addr.to_checksum(None),
            token = %token_in.to_checksum(None),
            spender = %cfg.router.to_checksum(None),
            current = %current_allowance,
            requested = %amount_in_wei,
            "[swap] approving router"
        );
        let data = encode_erc20_approve(cfg.router, amount_in_wei);
        let hash = rpc::send_tx_and_wait(
            chain,
            pk_bytes,
            Some(&format!("{:?}", token_in)),
            U256::ZERO,
            Some(data),
        )
        .await
        .map_err(|e| AppError::Transaction(format!("approve step failed: {e}")))?;
        tracing::info!(tx_hash = %hash, "[swap] approve confirmed");
        approve_tx_hash = Some(hash);
    } else {
        tracing::info!(
            allowance = %current_allowance,
            requested = %amount_in_wei,
            "[swap] sufficient allowance, skipping approve"
        );
    }

    //── step 3: exactinputsingle on uniswap v3 swaprouter02 ──────────
    tracing::info!(
        token_in = %token_in.to_checksum(None),
        token_out = %token_out.to_checksum(None),
        amount_in = %amount_in_wei,
        router = %cfg.router.to_checksum(None),
        "[swap] sending exactInputSingle"
    );
    let data = encode_exact_input_single(token_in, token_out, wallet_addr, amount_in_wei);
    let swap_tx_hash = rpc::send_tx_and_wait(
        chain,
        pk_bytes,
        Some(&format!("{:?}", cfg.router)),
        U256::ZERO,
        Some(data),
    )
    .await
    .map_err(|e| AppError::Transaction(format!("swap step failed: {e}")))?;
    tracing::info!(tx_hash = %swap_tx_hash, "[swap] swap confirmed");

    Ok(SwapResult {
        swap_tx_hash,
        wrap_tx_hash,
        approve_tx_hash,
    })
}
