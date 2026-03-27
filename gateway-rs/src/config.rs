use std::collections::HashMap;

use figment::{Figment, providers::{Env, Format, Toml}};
use serde::{Deserialize, Serialize};

fn default_host() -> String { "127.0.0.1".into() }
fn default_port() -> u16 { 9000 }
fn default_redis_url() -> String { "redis://127.0.0.1:6379".into() }
fn default_database_url() -> String { "sqlite://vespra.db".into() }
fn default_llm_provider() -> String { "deepseek".into() }
fn default_llm_model() -> String { "deepseek-chat".into() }
fn default_llm_base_url() -> String { "https://api.deepseek.com".into() }
fn default_price_oracle() -> String { "defillama".into() }
fn default_price_oracle_fallback() -> String { "coingecko".into() }
fn default_chains() -> Vec<String> { vec!["base".into(), "arbitrum".into()] }
fn default_trade_up_max_eth() -> f64 { 0.02 }
fn default_trade_up_cycle_interval_secs() -> u64 { 300 }
fn default_trade_up_min_gain_pct() -> f64 { 0.5 }
fn default_trade_up_stop_loss_pct() -> f64 { 5.0 }
fn default_auto_execute_max_eth() -> f64 { 0.05 }

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GatewayConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub keymaster_url: String,
    #[serde(default)]
    pub keymaster_token: String,
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    #[serde(default = "default_database_url")]
    pub database_url: String,
    #[serde(default = "default_llm_provider")]
    pub llm_provider: String,
    #[serde(default = "default_llm_model")]
    pub llm_model: String,
    #[serde(default)]
    pub llm_api_key: String,
    #[serde(default = "default_llm_base_url")]
    pub llm_base_url: String,
    #[serde(default = "default_price_oracle")]
    pub price_oracle: String,
    #[serde(default = "default_price_oracle_fallback")]
    pub price_oracle_fallback: String,
    #[serde(default)]
    pub price_oracle_api_key: Option<String>,
    #[serde(default)]
    pub price_oracle_base_url: Option<String>,
    #[serde(default = "default_chains")]
    pub chains: Vec<String>,
    #[serde(default)]
    pub trade_up_enabled: bool,
    #[serde(default = "default_trade_up_max_eth")]
    pub trade_up_max_eth: f64,
    #[serde(default = "default_trade_up_cycle_interval_secs")]
    pub trade_up_cycle_interval_secs: u64,
    #[serde(default = "default_trade_up_min_gain_pct")]
    pub trade_up_min_gain_pct: f64,
    #[serde(default = "default_trade_up_stop_loss_pct")]
    pub trade_up_stop_loss_pct: f64,
    #[serde(default)]
    pub yield_auto_rotate_enabled: bool,
    #[serde(default)]
    pub auto_execute_enabled: bool,
    #[serde(default = "default_auto_execute_max_eth")]
    pub auto_execute_max_eth: f64,
    #[serde(default)]
    pub oneinch_api_key: Option<String>,
    #[serde(default)]
    pub rpc_urls: HashMap<String, String>,
}

impl GatewayConfig {
    pub fn load() -> Result<Self, figment::Error> {
        let mut config: Self = Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::prefixed("VESPRA_"))
            .extract()?;

        // Scan env vars for RPC_URL_{CHAIN} and populate rpc_urls map
        for (key, val) in std::env::vars() {
            if let Some(chain) = key.strip_prefix("RPC_URL_") {
                let chain_name = chain.to_lowercase();
                config.rpc_urls.insert(chain_name, val);
            }
        }

        Ok(config)
    }
}
