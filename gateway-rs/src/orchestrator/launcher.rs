use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::agents::executor::ExecutorAgent;
use crate::agents::launcher::{LauncherAgent, LauncherContext};
use crate::config::GatewayConfig;
use crate::types::decisions::LaunchDecision;

//─── types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSpec {
    pub name: String,
    pub symbol: String,
    pub supply: u64,
    pub decimals: u8,
    pub chain: String,
    pub liquidity_eth: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchResult {
    pub deployed: bool,
    pub contract_address: Option<String>,
    pub tx_hash: Option<String>,
    pub liquidity_tx_hash: Option<String>,
    pub dex_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeployedContract {
    pub contract_address: String,
    pub name: String,
    pub symbol: String,
    pub supply: u64,
    pub decimals: u8,
    pub chain: String,
    pub deploy_tx_hash: String,
    pub liquidity_tx_hash: Option<String>,
    pub deployed_at: i64,
}

//─── erc-20 bytecode ─────────────────────────────────────────────

///standard openzeppelin erc-20 compiled bytecode (simplified).
///constructor: (string name, string symbol, uint256 totalsupply, uint8 decimals)
const ERC20_BYTECODE: &str = "608060405234801561001057600080fd5b506040516200\
0e3a3800620e3a833981016040819052610032916200016e565b8351849190620000\
4a906003906020870190620000b2565b50825162000060906004906020860190620000b2565b50600581905560068054\
60ff191660ff8416179055336000908152600160205260409020819055600581905560\
0080546001600160a01b03191633179055505050506200024b565b828054620000c090\
6200020e565b90600052602060002090601f016020900481019282620000e457600085\
5562000130565b82601f10620000ff57805160ff191683800117855562000130565b82\
80016001018555821562000130579182015b82811115620001305782518255916020019\
190600101905062000113565b506200013e929150620001425662000142565b5090565b\
5b808211156200013e576000815560010162000143565b634e487b7160e01b600052604\
160045260246000fd5b600082601f8301126200018057600080fd5b815160206001600160\
4018811163ffffffff1681146200019d57600080fd5b604051601f8301601f19168101810\
18381118382101715620001c257600080fd5b604052808452838382011115620001d857\
600080fd5b60005b83811015620001f8578581018301518282018401528201620001db56\
5b83811115620002095760008484015250505b509392505050565b600181811c908216806\
200022357607f821691505b60208210810362000244576000805160001960036101000a03\
1916909117905550565b5090565b610bdf806200025b6000396000f3fe";

//─── orchestrator ────────────────────────────────────────────────

pub struct LauncherOrchestrator {
    launcher_agent: Arc<LauncherAgent>,
    executor: Arc<ExecutorAgent>,
    config: Arc<GatewayConfig>,
    redis: Arc<redis::Client>,
    kill_flag: Arc<AtomicBool>,
}

impl LauncherOrchestrator {
    pub fn new(
        launcher_agent: Arc<LauncherAgent>,
        executor: Arc<ExecutorAgent>,
        config: Arc<GatewayConfig>,
        redis: Arc<redis::Client>,
        kill_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            launcher_agent,
            executor,
            config,
            redis,
            kill_flag,
        }
    }

    pub async fn deploy(&self, spec: TokenSpec, wallet_id: String) -> LaunchResult {
        //gate checks
        if self.kill_flag.load(Ordering::SeqCst) {
            return LaunchResult {
                deployed: false,
                contract_address: None,
                tx_hash: None,
                liquidity_tx_hash: None,
                dex_url: None,
                error: Some("kill_switch_active".into()),
            };
        }

        if !self.config.launcher_enabled {
            return LaunchResult {
                deployed: false,
                contract_address: None,
                tx_hash: None,
                liquidity_tx_hash: None,
                dex_url: None,
                error: Some("launcher_disabled".into()),
            };
        }

        //run launcher agent
        let liquidity_eth = spec.liquidity_eth
            .unwrap_or(self.config.launcher_initial_liquidity_eth);
        let agent_ctx = LauncherContext {
            name: spec.name.clone(),
            symbol: spec.symbol.clone(),
            supply: spec.supply,
            decimals: spec.decimals,
            chain: spec.chain.clone(),
            liquidity_eth,
        };
        let decision = match self.launcher_agent.evaluate(&agent_ctx).await {
            Ok(d) => d,
            Err(e) => {
                return LaunchResult {
                    deployed: false,
                    contract_address: None,
                    tx_hash: None,
                    liquidity_tx_hash: None,
                    dex_url: None,
                    error: Some(format!("launcher_agent_error: {e}")),
                };
            }
        };

        let suggested_liquidity = match decision {
            LaunchDecision::Rejected { reasoning } => {
                tracing::info!("launcher rejected: {reasoning}");
                return LaunchResult {
                    deployed: false,
                    contract_address: None,
                    tx_hash: None,
                    liquidity_tx_hash: None,
                    dex_url: None,
                    error: Some(format!("rejected: {reasoning}")),
                };
            }
            LaunchDecision::Approved { suggested_liquidity_eth, reasoning } => {
                tracing::info!("launcher approved: {reasoning}");
                suggested_liquidity_eth
            }
        };

        //abi-encode constructor arguments
        let constructor_args = encode_erc20_constructor(
            &spec.name,
            &spec.symbol,
            spec.supply,
            spec.decimals,
        );
        let deploy_data = format!("0x{}{}", ERC20_BYTECODE, hex::encode(&constructor_args));

        //deploy via keymaster (to="" for contract creation)
        let wallet_uuid = match uuid::Uuid::parse_str(&wallet_id) {
            Ok(id) => id,
            Err(e) => {
                return LaunchResult {
                    deployed: false,
                    contract_address: None,
                    tx_hash: None,
                    liquidity_tx_hash: None,
                    dex_url: None,
                    error: Some(format!("invalid wallet_id: {e}")),
                };
            }
        };

        let deploy_result = match self.executor
            .execute(wallet_uuid, &deploy_data, "", "0", &spec.chain)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return LaunchResult {
                    deployed: false,
                    contract_address: None,
                    tx_hash: None,
                    liquidity_tx_hash: None,
                    dex_url: None,
                    error: Some(format!("deploy_error: {e}")),
                };
            }
        };

        let deploy_tx_hash = match deploy_result.tx_hash {
            Some(h) => h,
            None => {
                return LaunchResult {
                    deployed: false,
                    contract_address: None,
                    tx_hash: None,
                    liquidity_tx_hash: None,
                    dex_url: None,
                    error: Some(deploy_result.error.unwrap_or_else(|| "no_tx_hash".into())),
                };
            }
        };

        //poll for receipt to get contract address
        let contract_address = self.poll_receipt(&deploy_tx_hash).await;

        let contract_addr = match contract_address {
            Some(addr) => addr,
            None => {
                return LaunchResult {
                    deployed: true,
                    contract_address: None,
                    tx_hash: Some(deploy_tx_hash),
                    liquidity_tx_hash: None,
                    dex_url: None,
                    error: Some("receipt_timeout — contract may still deploy".into()),
                };
            }
        };

        //add liquidity via keymaster
        let liq_amount_wei = format!("{:.0}", suggested_liquidity * 1e18);
        let liq_result = self.executor
            .execute(wallet_uuid, "WETH", &contract_addr, &liq_amount_wei, &spec.chain)
            .await;
        let liquidity_tx_hash = liq_result.ok().and_then(|r| r.tx_hash);

        let dex_url = format!("https://dexscreener.com/{}/{}", spec.chain, contract_addr);

        //persist to redis
        let deployed = DeployedContract {
            contract_address: contract_addr.clone(),
            name: spec.name,
            symbol: spec.symbol,
            supply: spec.supply,
            decimals: spec.decimals,
            chain: spec.chain,
            deploy_tx_hash: deploy_tx_hash.clone(),
            liquidity_tx_hash: liquidity_tx_hash.clone(),
            deployed_at: chrono::Utc::now().timestamp(),
        };
        self.persist_contract(&deployed).await;

        LaunchResult {
            deployed: true,
            contract_address: Some(contract_addr),
            tx_hash: Some(deploy_tx_hash),
            liquidity_tx_hash,
            dex_url: Some(dex_url),
            error: None,
        }
    }

    async fn poll_receipt(&self, tx_hash: &str) -> Option<String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let url = format!("{}/tx/{}/receipt", self.config.keymaster_url, tx_hash);
        let token = &self.config.keymaster_token;

        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            if let Ok(resp) = client
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await
            {
                if let Ok(data) = resp.json::<serde_json::Value>().await {
                    if let Some(addr) = data.get("contract_address").and_then(|v| v.as_str()) {
                        if !addr.is_empty() {
                            return Some(addr.to_string());
                        }
                    }
                }
            }
        }
        None
    }

    async fn persist_contract(&self, contract: &DeployedContract) {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            if let Ok(json) = serde_json::to_string(contract) {
                let _: Result<(), _> = conn.hset::<_, _, _, ()>(
                    "vespra:deployed_contracts",
                    &contract.contract_address,
                    &json,
                ).await;
            }
        }
    }

    pub async fn list_contracts(&self) -> Vec<serde_json::Value> {
        if let Ok(mut conn) = redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await {
            let all: std::collections::HashMap<String, String> = conn
                .hgetall("vespra:deployed_contracts")
                .await
                .unwrap_or_default();
            all.values()
                .filter_map(|s| serde_json::from_str(s).ok())
                .collect()
        } else {
            vec![]
        }
    }

    pub async fn get_contract(&self, address: &str) -> Option<serde_json::Value> {
        let mut conn = redis::Client::get_multiplexed_async_connection(self.redis.as_ref())
            .await
            .ok()?;
        let raw: Option<String> = conn.hget("vespra:deployed_contracts", address).await.ok().flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }
}

//─── abi encoding ────────────────────────────────────────────────

fn encode_erc20_constructor(name: &str, symbol: &str, supply: u64, decimals: u8) -> Vec<u8> {
    use ethabi::{encode, Token};

    let total_supply = ethabi::ethereum_types::U256::from(supply)
        * ethabi::ethereum_types::U256::exp10(decimals as usize);

    encode(&[
        Token::String(name.to_string()),
        Token::String(symbol.to_string()),
        Token::Uint(total_supply),
        Token::Uint(ethabi::ethereum_types::U256::from(decimals)),
    ])
}
