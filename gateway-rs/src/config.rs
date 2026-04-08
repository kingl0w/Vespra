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
fn default_trade_up_target_gain_pct() -> f64 { 15.0 }
fn default_trade_up_gas_reserve_eth() -> f64 { 0.01 }
fn default_auto_execute_max_eth() -> f64 { 0.05 }
fn default_cors_origin() -> String { "*".into() }
fn default_nullboiler_url() -> String { "http://127.0.0.1:9090".into() }
fn default_rl_webhook_rpm() -> u64 { 60 }
fn default_yield_auto_rotate_threshold_pct() -> f64 { 1.0 }
fn default_yield_max_rotate_eth() -> f64 { 0.05 }
fn default_yield_cycle_interval_secs() -> u64 { 3600 }
fn default_sniper_max_entry_eth() -> f64 { 0.05 }
fn default_sniper_min_tvl() -> f64 { 50000.0 }
fn default_sniper_exit_tvl_drop_pct() -> f64 { 30.0 }
fn default_sniper_target_gain_pct() -> f64 { 15.0 }
fn default_sniper_stop_loss_pct() -> f64 { 8.0 }
fn default_launcher_initial_liquidity_eth() -> f64 { 0.05 }
fn default_custody() -> String { "safe".into() }
fn default_trader_max_slippage_pct() -> f64 { 1.0 }
fn default_volatility_gate_threshold() -> f64 { 15.0 }
fn default_rate_limit_enabled() -> bool { true }
fn default_rate_limit_agent_rpm() -> u32 { 10 }
fn default_rate_limit_wallet_create_rph() -> u32 { 5 }
fn default_rate_limit_tx_send_rph() -> u32 { 20 }
fn default_yield_providers() -> String { "defillama".into() }
fn default_yield_min_tvl_usd() -> f64 { 500_000.0 }
fn default_yield_min_apy() -> f64 { 1.0 }
fn default_yield_top_n() -> usize { 20 }

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
    #[serde(default = "default_trade_up_target_gain_pct")]
    pub trade_up_target_gain_pct: f64,
    #[serde(default = "default_trade_up_gas_reserve_eth")]
    pub trade_up_gas_reserve_eth: f64,
    #[serde(default)]
    pub yield_auto_rotate_enabled: bool,
    #[serde(default = "default_yield_auto_rotate_threshold_pct")]
    pub yield_auto_rotate_threshold_pct: f64,
    #[serde(default = "default_yield_max_rotate_eth")]
    pub yield_max_rotate_eth: f64,
    #[serde(default = "default_yield_cycle_interval_secs")]
    pub yield_cycle_interval_secs: u64,
    #[serde(default)]
    pub sniper_auto_entry_enabled: bool,
    #[serde(default = "default_sniper_max_entry_eth")]
    pub sniper_max_entry_eth: f64,
    #[serde(default = "default_sniper_min_tvl")]
    pub sniper_min_tvl: f64,
    #[serde(default = "default_sniper_exit_tvl_drop_pct")]
    pub sniper_exit_tvl_drop_pct: f64,
    #[serde(default = "default_sniper_target_gain_pct")]
    pub sniper_target_gain_pct: f64,
    #[serde(default = "default_sniper_stop_loss_pct")]
    pub sniper_stop_loss_pct: f64,
    #[serde(default)]
    pub alchemy_webhook_secret: String,
    #[serde(default)]
    pub launcher_enabled: bool,
    #[serde(default = "default_launcher_initial_liquidity_eth")]
    pub launcher_initial_liquidity_eth: f64,
    #[serde(default = "default_custody")]
    pub default_custody: String,
    #[serde(default)]
    pub auto_execute_enabled: bool,
    #[serde(default = "default_auto_execute_max_eth")]
    pub auto_execute_max_eth: f64,
    #[serde(default)]
    pub oneinch_api_key: Option<String>,
    #[serde(default)]
    pub paraswap_mode: bool,
    #[serde(default = "default_cors_origin")]
    pub cors_origin: String,
    #[serde(default)]
    pub cf_access_required: bool,
    #[serde(default = "default_nullboiler_url")]
    pub nullboiler_url: String,
    #[serde(default = "default_rl_webhook_rpm")]
    pub rl_webhook_rpm: u64,
    #[serde(default = "default_trader_max_slippage_pct")]
    pub trader_max_slippage_pct: f64,
    #[serde(default = "default_volatility_gate_threshold")]
    pub volatility_gate_threshold: f64,
    #[serde(default = "default_yield_providers")]
    pub yield_providers: String,
    #[serde(default = "default_yield_min_tvl_usd")]
    pub yield_min_tvl_usd: f64,
    #[serde(default = "default_yield_min_apy")]
    pub yield_min_apy: f64,
    #[serde(default = "default_yield_top_n")]
    pub yield_top_n: usize,
    #[serde(default = "default_rate_limit_enabled")]
    pub rate_limit_enabled: bool,
    #[serde(default = "default_rate_limit_agent_rpm")]
    pub rate_limit_agent_rpm: u32,
    #[serde(default = "default_rate_limit_wallet_create_rph")]
    pub rate_limit_wallet_create_rph: u32,
    #[serde(default = "default_rate_limit_tx_send_rph")]
    pub rate_limit_tx_send_rph: u32,
    #[serde(default)]
    pub rpc_url_override: Option<String>,
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

        // VES-110: explicit GATEWAY_HOST / GATEWAY_PORT env-var overrides on
        // top of whatever Figment merged from TOML / VESPRA_*. This makes
        // container deployments configurable without a config.toml file.
        if let Ok(host) = std::env::var("GATEWAY_HOST") {
            if !host.is_empty() {
                config.host = host;
            }
        }
        if let Ok(port_str) = std::env::var("GATEWAY_PORT") {
            match port_str.parse::<u16>() {
                Ok(p) => config.port = p,
                Err(e) => tracing::warn!(
                    "GATEWAY_PORT='{port_str}' is not a valid u16 ({e}) — keeping {}",
                    config.port
                ),
            }
        }
        tracing::info!(
            "gateway-rs config resolved: host={} port={}",
            config.host,
            config.port
        );

        Ok(config)
    }
}
