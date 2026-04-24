use std::collections::HashMap;

use figment::{Figment, providers::{Env, Format, Toml}};
use serde::{Deserialize, Serialize};

/// Deserializes a value that may arrive as a string or an integer into Option<String>.
/// Needed because env vars like VESPRA_TELEGRAM_CHAT_ID contain numeric values that
/// Figment parses as integers, but we store them as strings.
fn deserialize_string_or_int<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct StringOrInt;

    impl<'de> de::Visitor<'de> for StringOrInt {
        type Value = Option<String>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or integer")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            if v.is_empty() {
                Ok(None)
            } else {
                Ok(Some(v.to_string()))
            }
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            if v.is_empty() {
                Ok(None)
            } else {
                Ok(Some(v))
            }
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v.to_string()))
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
    }

    deserializer.deserialize_any(StringOrInt)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkMode {
    Testnet,
    Mainnet,
}

impl Default for NetworkMode {
    fn default() -> Self {
        NetworkMode::Testnet
    }
}

impl serde::Serialize for NetworkMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            NetworkMode::Testnet => serializer.serialize_str("testnet"),
            NetworkMode::Mainnet => serializer.serialize_str("mainnet"),
        }
    }
}

impl<'de> serde::Deserialize<'de> for NetworkMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.to_lowercase().as_str() {
            "testnet" => Ok(NetworkMode::Testnet),
            "mainnet" => Ok(NetworkMode::Mainnet),
            _ => Err(serde::de::Error::custom(format!(
                "invalid network mode '{}' — must be 'testnet' or 'mainnet'",
                s
            ))),
        }
    }
}

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
fn default_testnet_monitor_timeout_minutes() -> u64 { 5 }
fn default_max_tx_per_hour() -> Option<u32> { Some(100) }

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
    #[serde(default = "default_testnet_monitor_timeout_minutes")]
    pub testnet_monitor_timeout_minutes: u64,
    #[serde(default)]
    pub rpc_url_override: Option<String>,
    #[serde(default)]
    pub rpc_urls: HashMap<String, String>,
    #[serde(default)]
    pub telegram_bot_token: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    pub telegram_chat_id: Option<String>,
    #[serde(default)]
    pub network_mode: NetworkMode,
    #[serde(default)]
    pub max_global_wallet_value_eth: Option<f64>,
    #[serde(default = "default_max_tx_per_hour")]
    pub max_tx_per_hour: Option<u32>,
}

impl GatewayConfig {
    pub fn load() -> Result<Self, figment::Error> {
        let mut config: Self = Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::prefixed("VESPRA_"))
            .extract()?;

        //scan env vars for rpc_url_{chain} and populate rpc_urls map
        for (key, val) in std::env::vars() {
            if let Some(chain) = key.strip_prefix("RPC_URL_") {
                let chain_name = chain.to_lowercase();
                config.rpc_urls.insert(chain_name, val);
            }
        }

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

    pub fn is_testnet(&self) -> bool {
        matches!(self.network_mode, NetworkMode::Testnet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_mode_default_is_testnet() {
        assert_eq!(NetworkMode::default(), NetworkMode::Testnet);
    }

    #[test]
    fn network_mode_parses_case_insensitive() {
        for s in ["mainnet", "MAINNET", "Mainnet", "MainNet"] {
            let json = format!("\"{}\"", s);
            let parsed: NetworkMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, NetworkMode::Mainnet, "failed to parse {s}");
        }
        for s in ["testnet", "TESTNET", "Testnet"] {
            let json = format!("\"{}\"", s);
            let parsed: NetworkMode = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, NetworkMode::Testnet, "failed to parse {s}");
        }
    }

    #[test]
    fn network_mode_rejects_invalid() {
        let result: Result<NetworkMode, _> = serde_json::from_str("\"foo\"");
        let err = result.expect_err("expected error for invalid value");
        let msg = err.to_string();
        assert!(
            msg.contains("testnet") && msg.contains("mainnet"),
            "error should list valid values, got: {msg}"
        );
    }
}
