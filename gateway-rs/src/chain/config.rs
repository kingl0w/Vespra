use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainConfig {
    pub name: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub native_token: String,
    pub native_token_address: String,
    pub defillama_slug: String,
    pub coingecko_id: String,
    pub supported_dexes: Vec<String>,
    pub explorer_url: String,
}
