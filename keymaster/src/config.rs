use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub db_path: PathBuf,
    pub chains: HashMap<String, ChainConfig>,
    #[serde(default)]
    pub fees_enabled: bool,
    #[serde(default)]
    pub treasury_address: Option<String>,
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
            fees_enabled: false,
            treasury_address: None,
        }
    }
}

impl Config {
    ///load config, override rpc urls and safe addresses from env vars.
    ///env pattern: vespra_rpc_ethereum, vespra_safe_base, etc.
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

        if let Ok(v) = std::env::var("FEES_ENABLED") {
            config.fees_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = std::env::var("TREASURY_ADDRESS") {
            if !v.is_empty() {
                config.treasury_address = Some(v);
            }
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

/// Validate that a string looks like a valid EVM address: 0x + 40 hex chars.
pub fn is_valid_evm_address(addr: &str) -> bool {
    addr.len() == 42
        && addr.starts_with("0x")
        && addr[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Validate fee config at boot time. Returns Err with a human-readable
/// message if fees_enabled but treasury_address is missing or invalid.
pub fn validate_fee_config(config: &Config) -> Result<(), String> {
    if !config.fees_enabled {
        return Ok(());
    }
    match &config.treasury_address {
        None => Err(
            "FEES_ENABLED=true but TREASURY_ADDRESS is not set — refusing to start".into(),
        ),
        Some(addr) if addr.is_empty() => Err(
            "FEES_ENABLED=true but TREASURY_ADDRESS is not set — refusing to start".into(),
        ),
        Some(addr) if !is_valid_evm_address(addr) => Err(format!(
            "TREASURY_ADDRESS invalid: {addr} — must be a valid EVM address (0x + 40 hex chars)"
        )),
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fees_disabled_by_default() {
        let config = Config::default();
        assert!(!config.fees_enabled);
        assert!(config.treasury_address.is_none());
    }

    #[test]
    fn treasury_address_none_when_unset() {
        let config = Config::default();
        assert!(config.treasury_address.is_none());
    }

    #[test]
    fn is_valid_evm_address_accepts_good() {
        assert!(is_valid_evm_address(
            "0x0000000000000000000000000000000000000000"
        ));
        assert!(is_valid_evm_address(
            "0xAbCdEf0123456789abcdef0123456789ABCDEF01"
        ));
    }

    #[test]
    fn is_valid_evm_address_rejects_bad() {
        assert!(!is_valid_evm_address(""));
        assert!(!is_valid_evm_address("0x"));
        assert!(!is_valid_evm_address("0x123")); // too short
        assert!(!is_valid_evm_address(
            "0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"
        )); // non-hex
        assert!(!is_valid_evm_address(
            "0000000000000000000000000000000000000000000"
        )); // no 0x prefix
    }

    #[test]
    fn validate_ok_when_fees_disabled() {
        let config = Config {
            treasury_address: None,
            ..Config::default()
        };
        assert!(validate_fee_config(&config).is_ok());
    }

    #[test]
    fn validate_fails_when_fees_enabled_no_treasury() {
        let config = Config {
            fees_enabled: true,
            treasury_address: None,
            ..Config::default()
        };
        let err = validate_fee_config(&config).unwrap_err();
        assert!(err.contains("TREASURY_ADDRESS is not set"));
    }

    #[test]
    fn validate_fails_when_fees_enabled_invalid_treasury() {
        let config = Config {
            fees_enabled: true,
            treasury_address: Some("not-an-address".into()),
            ..Config::default()
        };
        let err = validate_fee_config(&config).unwrap_err();
        assert!(err.contains("invalid"));
    }

    #[test]
    fn validate_ok_when_fees_enabled_valid_treasury() {
        let config = Config {
            fees_enabled: true,
            treasury_address: Some(
                "0x0000000000000000000000000000000000000001".into(),
            ),
            ..Config::default()
        };
        assert!(validate_fee_config(&config).is_ok());
    }
}
