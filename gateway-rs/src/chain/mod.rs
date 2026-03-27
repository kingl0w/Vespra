pub mod config;

use self::config::ChainConfig;
use std::collections::HashMap;

pub struct ChainRegistry {
    chains: HashMap<String, ChainConfig>,
}

impl ChainRegistry {
    /// Create registry with built-in chains. RPC URLs are loaded from the provided map
    /// (keys are lowercase chain names, e.g. "base", "arbitrum").
    pub fn new(rpc_urls: &HashMap<String, String>) -> Self {
        let builtins = [
            ChainConfig {
                name: "base".into(),
                chain_id: 8453,
                rpc_url: String::new(),
                native_token: "ETH".into(),
                native_token_address: "0x4200000000000000000000000000000000000006".into(),
                defillama_slug: "base".into(),
                coingecko_id: "base".into(),
                supported_dexes: vec![
                    "aerodrome-slipstream".into(),
                    "aerodrome-v1".into(),
                    "uniswap-v3".into(),
                    "uniswap-v4".into(),
                ],
                explorer_url: "https://basescan.org".into(),
            },
            ChainConfig {
                name: "arbitrum".into(),
                chain_id: 42161,
                rpc_url: String::new(),
                native_token: "ETH".into(),
                native_token_address: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".into(),
                defillama_slug: "arbitrum".into(),
                coingecko_id: "arbitrum-one".into(),
                supported_dexes: vec![
                    "uniswap-v3".into(),
                    "pendle".into(),
                    "camelot".into(),
                ],
                explorer_url: "https://arbiscan.io".into(),
            },
            ChainConfig {
                name: "ethereum".into(),
                chain_id: 1,
                rpc_url: String::new(),
                native_token: "ETH".into(),
                native_token_address: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".into(),
                defillama_slug: "ethereum".into(),
                coingecko_id: "ethereum".into(),
                supported_dexes: vec![
                    "uniswap-v3".into(),
                    "uniswap-v4".into(),
                    "curve".into(),
                ],
                explorer_url: "https://etherscan.io".into(),
            },
            ChainConfig {
                name: "optimism".into(),
                chain_id: 10,
                rpc_url: String::new(),
                native_token: "ETH".into(),
                native_token_address: "0x4200000000000000000000000000000000000006".into(),
                defillama_slug: "optimism".into(),
                coingecko_id: "optimistic-ethereum".into(),
                supported_dexes: vec![
                    "velodrome".into(),
                    "uniswap-v3".into(),
                ],
                explorer_url: "https://optimistic.etherscan.io".into(),
            },
            ChainConfig {
                name: "polygon".into(),
                chain_id: 137,
                rpc_url: String::new(),
                native_token: "MATIC".into(),
                native_token_address: "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270".into(),
                defillama_slug: "polygon".into(),
                coingecko_id: "polygon-pos".into(),
                supported_dexes: vec![
                    "quickswap".into(),
                    "uniswap-v3".into(),
                ],
                explorer_url: "https://polygonscan.com".into(),
            },
            ChainConfig {
                name: "base_sepolia".into(),
                chain_id: 84532,
                rpc_url: String::new(),
                native_token: "ETH".into(),
                native_token_address: "0x4200000000000000000000000000000000000006".into(),
                defillama_slug: "base".into(),
                coingecko_id: "base".into(),
                supported_dexes: vec![
                    "aerodrome-slipstream".into(),
                ],
                explorer_url: "https://sepolia.basescan.org".into(),
            },
        ];

        let mut chains = HashMap::new();
        for mut chain in builtins {
            // Set rpc_url from the provided map
            if let Some(url) = rpc_urls.get(&chain.name) {
                chain.rpc_url = url.clone();
            }
            chains.insert(chain.name.clone(), chain);
        }

        Self { chains }
    }

    pub fn get(&self, name: &str) -> Option<&ChainConfig> {
        self.chains.get(name)
    }

    /// Returns only chains that have an RPC URL configured.
    pub fn available(&self) -> Vec<&ChainConfig> {
        let mut out: Vec<_> = self.chains.values()
            .filter(|c| !c.rpc_url.is_empty())
            .collect();
        out.sort_by_key(|c| c.chain_id);
        out
    }

    pub fn defillama_slug(&self, name: &str) -> Option<&str> {
        self.chains.get(name).map(|c| c.defillama_slug.as_str())
    }

    pub fn chain_id(&self, name: &str) -> Option<u64> {
        self.chains.get(name).map(|c| c.chain_id)
    }

    /// Reverse lookup: find a chain whose defillama_slug matches (case-insensitive).
    pub fn from_defillama_slug(&self, slug: &str) -> Option<&ChainConfig> {
        let slug_lower = slug.to_lowercase();
        self.chains.values().find(|c| c.defillama_slug.to_lowercase() == slug_lower)
    }
}
