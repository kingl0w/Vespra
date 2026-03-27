use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EntrySignal {
    Strong,
    Moderate,
    Weak,
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
#[allow(dead_code)]
pub enum RiskTier {
    Low,
    Medium,
    #[default]
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    pub protocol: String,
    pub pool: String,
    pub chain: String,
    #[serde(default)]
    pub apy: f64,
    #[serde(default)]
    pub tvl_usd: u64,
    #[serde(default)]
    pub momentum_score: f64,
    #[serde(default)]
    pub entry_signal: EntrySignal,
    #[serde(default)]
    pub price_change_24h_pct: f64,
    #[serde(default)]
    pub price_usd: f64,
    #[serde(default)]
    pub risk_tier: RiskTier,
    #[serde(default)]
    pub il_risk: bool,
    #[serde(default)]
    pub volume_24h: u64,
    #[serde(default)]
    pub volume_spike_pct: f64,
    #[serde(default)]
    pub tvl_change_7d_pct: f64,
}

impl Opportunity {
    pub fn is_yield_position(&self) -> bool {
        self.apy >= 50.0 && self.price_change_24h_pct == 0.0
    }

    pub fn expected_yield_gain_pct(&self, cycle_interval_secs: u64) -> f64 {
        (self.apy / 365.0 / 24.0 / 60.0) * (cycle_interval_secs as f64 / 60.0)
    }
}
