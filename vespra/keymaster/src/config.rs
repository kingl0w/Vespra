use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub db_path: PathBuf,
    pub chains: HashMap<String, ChainConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub rpc_url: String,
    pub safe_address: Option<String>,
    pub explorer_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let mut chains = HashMap::new();

        chains.insert("ethereum".into(), ChainConfig {
            chain_id: 1,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://etherscan.io".into()),
        });

        chains.insert("base".into(), ChainConfig {
            chain_id: 8453,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://basescan.org".into()),
        });

        chains.insert("arbitrum".into(), ChainConfig {
            chain_id: 42161,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://arbiscan.io".into()),
        });

        chains.insert("optimism".into(), ChainConfig {
            chain_id: 10,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://optimistic.etherscan.io".into()),
        });

        chains.insert("sepolia".into(), ChainConfig {
            chain_id: 11155111,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://sepolia.etherscan.io".into()),
        });

        chains.insert("base_sepolia".into(), ChainConfig {
            chain_id: 84532,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://sepolia.basescan.org".into()),
        });

        chains.insert("arbitrum_sepolia".into(), ChainConfig {
            chain_id: 421614,
            rpc_url: String::new(),
            safe_address: None,
            explorer_url: Some("https://sepolia.arbiscan.io".into()),
        });

        Self {
            host: "127.0.0.1".into(),
            port: 9100,
            db_path: PathBuf::from("keymaster.db"),
            chains,
        }
    }
}

impl Config {
    /// Load config, override RPC URLs and Safe addresses from env vars.
    /// Env pattern: VESPRA_RPC_ETHEREUM, VESPRA_SAFE_BASE, etc.
    pub fn load(path: Option<&Path>) -> Self {
        let mut config = if let Some(p) = path {
            if p.exists() {
                let contents = std::fs::read_to_string(p).unwrap_or_default();
                serde_json::from_str(&contents).unwrap_or_default()
            } else {
                Config::default()
            }
        } else {
            Config::default()
        };

        if let Ok(host) = std::env::var("VESPRA_KM_HOST") {
            config.host = host;
        }
        if let Ok(port) = std::env::var("VESPRA_KM_PORT") {
            if let Ok(p) = port.parse() {
                config.port = p;
            }
        }
        if let Ok(db) = std::env::var("VESPRA_KM_DB") {
            config.db_path = PathBuf::from(db);
        }

        for (name, chain) in config.chains.iter_mut() {
            let upper = name.to_uppercase();
            if let Ok(rpc) = std::env::var(format!("VESPRA_RPC_{upper}")) {
                chain.rpc_url = rpc;
            }
            if let Ok(safe) = std::env::var(format!("VESPRA_SAFE_{upper}")) {
                chain.safe_address = Some(safe);
            }
        }

        config
    }

    pub fn get_chain(&self, name: &str) -> Option<&ChainConfig> {
        self.chains.get(name).filter(|c| !c.rpc_url.is_empty())
    }

    pub fn active_chains(&self) -> Vec<(&String, &ChainConfig)> {
        self.chains
            .iter()
            .filter(|(_, c)| !c.rpc_url.is_empty())
            .collect()
    }
}
