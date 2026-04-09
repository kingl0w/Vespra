use anyhow::Result;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};

use crate::config::GatewayConfig;

//── redis key helper ───────────────────────────────────────────

fn redis_key(agent: &str) -> String {
    format!("agent_config:{agent}")
}

///known agent names.
pub const AGENT_NAMES: &[&str] = &[
    "scout", "risk", "trader", "sentinel", "sniper", "yield", "tradeup",
];

//── per-agent config structs ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoutConfig {
    pub min_tvl_usd: f64,
    pub min_apy_pct: f64,
    pub preferred_protocols: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_risk_tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderConfig {
    pub max_slippage_pct: f64,
    pub preferred_dex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelConfig {
    pub interval_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SniperConfig {
    pub max_position_eth: f64,
    pub auto_entry: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YieldConfig {
    pub rotation_threshold_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeUpConfig {
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
}

//── defaults from gatewayconfig ────────────────────────────────

impl ScoutConfig {
    pub fn defaults(cfg: &GatewayConfig) -> Self {
        Self {
            min_tvl_usd: cfg.yield_min_tvl_usd,
            min_apy_pct: cfg.yield_min_apy,
            preferred_protocols: vec![],
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["min_tvl_usd", "min_apy_pct", "preferred_protocols"]
    }
}

impl RiskConfig {
    pub fn defaults(_cfg: &GatewayConfig) -> Self {
        Self {
            max_risk_tier: "MEDIUM".into(),
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["max_risk_tier"]
    }
}

impl TraderConfig {
    pub fn defaults(cfg: &GatewayConfig) -> Self {
        Self {
            max_slippage_pct: cfg.trader_max_slippage_pct,
            preferred_dex: "auto".into(),
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["max_slippage_pct", "preferred_dex"]
    }
}

impl SentinelConfig {
    pub fn defaults(_cfg: &GatewayConfig) -> Self {
        Self {
            interval_secs: 300,
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["interval_secs"]
    }
}

impl SniperConfig {
    pub fn defaults(cfg: &GatewayConfig) -> Self {
        Self {
            max_position_eth: cfg.sniper_max_entry_eth,
            auto_entry: cfg.sniper_auto_entry_enabled,
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["max_position_eth", "auto_entry"]
    }
}

impl YieldConfig {
    pub fn defaults(cfg: &GatewayConfig) -> Self {
        Self {
            rotation_threshold_pct: cfg.yield_auto_rotate_threshold_pct,
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["rotation_threshold_pct"]
    }
}

impl TradeUpConfig {
    pub fn defaults(cfg: &GatewayConfig) -> Self {
        Self {
            take_profit_pct: cfg.trade_up_target_gain_pct,
            stop_loss_pct: cfg.trade_up_stop_loss_pct,
        }
    }

    pub fn known_fields() -> &'static [&'static str] {
        &["take_profit_pct", "stop_loss_pct"]
    }
}

//── load / save ────────────────────────────────────────────────

///load agent config from redis, falling back to defaults from gatewayconfig.
pub async fn load_agent_config(
    redis: &redis::Client,
    agent: &str,
    cfg: &GatewayConfig,
) -> Result<serde_json::Value> {
    let mut conn = redis::Client::get_multiplexed_async_connection(redis).await?;
    let raw: Option<String> = conn.get(redis_key(agent)).await?;

    if let Some(raw) = raw {
        Ok(serde_json::from_str(&raw)?)
    } else {
        Ok(default_config_json(agent, cfg))
    }
}

///load all agent configs.
pub async fn load_all_configs(
    redis: &redis::Client,
    cfg: &GatewayConfig,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut map = serde_json::Map::new();
    for &name in AGENT_NAMES {
        let val = load_agent_config(redis, name, cfg).await?;
        map.insert(name.to_string(), val);
    }
    Ok(map)
}

///save agent config to redis.
pub async fn save_agent_config(
    redis: &redis::Client,
    agent: &str,
    config: &serde_json::Value,
) -> Result<()> {
    let mut conn = redis::Client::get_multiplexed_async_connection(redis).await?;
    let json = serde_json::to_string(config)?;
    conn.set::<_, _, ()>(redis_key(agent), &json).await?;
    Ok(())
}

///return default config json for a given agent.
pub fn default_config_json(agent: &str, cfg: &GatewayConfig) -> serde_json::Value {
    match agent {
        "scout" => serde_json::to_value(ScoutConfig::defaults(cfg)).unwrap(),
        "risk" => serde_json::to_value(RiskConfig::defaults(cfg)).unwrap(),
        "trader" => serde_json::to_value(TraderConfig::defaults(cfg)).unwrap(),
        "sentinel" => serde_json::to_value(SentinelConfig::defaults(cfg)).unwrap(),
        "sniper" => serde_json::to_value(SniperConfig::defaults(cfg)).unwrap(),
        "yield" => serde_json::to_value(YieldConfig::defaults(cfg)).unwrap(),
        "tradeup" => serde_json::to_value(TradeUpConfig::defaults(cfg)).unwrap(),
        _ => serde_json::json!({}),
    }
}

///return known field names for an agent.
pub fn known_fields(agent: &str) -> Option<&'static [&'static str]> {
    match agent {
        "scout" => Some(ScoutConfig::known_fields()),
        "risk" => Some(RiskConfig::known_fields()),
        "trader" => Some(TraderConfig::known_fields()),
        "sentinel" => Some(SentinelConfig::known_fields()),
        "sniper" => Some(SniperConfig::known_fields()),
        "yield" => Some(YieldConfig::known_fields()),
        "tradeup" => Some(TradeUpConfig::known_fields()),
        _ => None,
    }
}

///validate numeric ranges for agent config values.
pub fn validate_patch(agent: &str, patch: &serde_json::Value) -> Result<(), String> {
    let obj = patch.as_object().ok_or("body must be a JSON object")?;

    //reject unknown fields
    if let Some(fields) = known_fields(agent) {
        for key in obj.keys() {
            if !fields.contains(&key.as_str()) {
                return Err(format!("unknown field '{key}' for agent '{agent}'"));
            }
        }
    } else {
        return Err(format!("unknown agent '{agent}'"));
    }

    //validate numeric ranges
    match agent {
        "trader" => {
            if let Some(v) = obj.get("max_slippage_pct").and_then(|v| v.as_f64()) {
                if !(0.1..=50.0).contains(&v) {
                    return Err("max_slippage_pct must be between 0.1 and 50.0".into());
                }
            }
        }
        "scout" => {
            if let Some(v) = obj.get("min_tvl_usd").and_then(|v| v.as_f64()) {
                if v < 0.0 {
                    return Err("min_tvl_usd must be >= 0".into());
                }
            }
            if let Some(v) = obj.get("min_apy_pct").and_then(|v| v.as_f64()) {
                if v < 0.0 {
                    return Err("min_apy_pct must be >= 0".into());
                }
            }
        }
        "sniper" => {
            if let Some(v) = obj.get("max_position_eth").and_then(|v| v.as_f64()) {
                if !(0.001..=10.0).contains(&v) {
                    return Err("max_position_eth must be between 0.001 and 10.0".into());
                }
            }
        }
        "sentinel" => {
            if let Some(v) = obj.get("interval_secs").and_then(|v| v.as_u64()) {
                if !(10..=86400).contains(&v) {
                    return Err("interval_secs must be between 10 and 86400".into());
                }
            }
        }
        "tradeup" => {
            if let Some(v) = obj.get("take_profit_pct").and_then(|v| v.as_f64()) {
                if !(0.1..=100.0).contains(&v) {
                    return Err("take_profit_pct must be between 0.1 and 100.0".into());
                }
            }
            if let Some(v) = obj.get("stop_loss_pct").and_then(|v| v.as_f64()) {
                if !(0.1..=100.0).contains(&v) {
                    return Err("stop_loss_pct must be between 0.1 and 100.0".into());
                }
            }
        }
        "risk" => {
            if let Some(v) = obj.get("max_risk_tier").and_then(|v| v.as_str()) {
                if !["LOW", "MEDIUM", "HIGH"].contains(&v) {
                    return Err("max_risk_tier must be LOW, MEDIUM, or HIGH".into());
                }
            }
        }
        "yield" => {
            if let Some(v) = obj.get("rotation_threshold_pct").and_then(|v| v.as_f64()) {
                if !(0.01..=50.0).contains(&v) {
                    return Err("rotation_threshold_pct must be between 0.01 and 50.0".into());
                }
            }
        }
        _ => {}
    }

    Ok(())
}

//── tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_field_rejected() {
        let patch = serde_json::json!({ "unknown_field": 42 });
        let result = validate_patch("scout", &patch);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown field"));
    }

    #[test]
    fn unknown_agent_rejected() {
        let patch = serde_json::json!({ "foo": 1 });
        let result = validate_patch("nonexistent", &patch);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown agent"));
    }

    #[test]
    fn valid_fields_accepted() {
        let patch = serde_json::json!({ "min_tvl_usd": 100000.0 });
        assert!(validate_patch("scout", &patch).is_ok());

        let patch = serde_json::json!({ "max_slippage_pct": 2.5 });
        assert!(validate_patch("trader", &patch).is_ok());
    }

    #[test]
    fn slippage_range_validated() {
        let too_low = serde_json::json!({ "max_slippage_pct": 0.01 });
        assert!(validate_patch("trader", &too_low).is_err());

        let too_high = serde_json::json!({ "max_slippage_pct": 99.0 });
        assert!(validate_patch("trader", &too_high).is_err());

        let ok = serde_json::json!({ "max_slippage_pct": 1.5 });
        assert!(validate_patch("trader", &ok).is_ok());
    }

    #[test]
    fn risk_tier_validated() {
        let ok = serde_json::json!({ "max_risk_tier": "LOW" });
        assert!(validate_patch("risk", &ok).is_ok());

        let bad = serde_json::json!({ "max_risk_tier": "EXTREME" });
        assert!(validate_patch("risk", &bad).is_err());
    }

    #[test]
    fn defaults_populated_from_config() {
        let cfg: GatewayConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        let scout = ScoutConfig::defaults(&cfg);
        assert!(scout.min_tvl_usd > 0.0);
        assert!(scout.min_apy_pct > 0.0);

        let trader = TraderConfig::defaults(&cfg);
        assert!(trader.max_slippage_pct > 0.0);

        let sniper = SniperConfig::defaults(&cfg);
        assert!(sniper.max_position_eth > 0.0);
    }

    #[test]
    fn config_persists_via_json_roundtrip() {
        //simulates: save to redis as json, load back — verifying serde roundtrip
        let cfg: GatewayConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        for &agent in AGENT_NAMES {
            let original = default_config_json(agent, &cfg);
            let serialized = serde_json::to_string(&original).unwrap();
            let deserialized: serde_json::Value = serde_json::from_str(&serialized).unwrap();
            assert_eq!(original, deserialized, "roundtrip failed for agent '{agent}'");
        }
    }
}
