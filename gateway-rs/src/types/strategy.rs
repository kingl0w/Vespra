use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub enum StrategyType {
    YieldRotation,
    Momentum,
    Arbitrage,
    TradeUp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub id: uuid::Uuid,
    pub name: String,
    pub strategy_type: StrategyType,
    pub chains: Vec<String>,
    pub protocols: Vec<String>,
    pub wallet_id: uuid::Uuid,
    pub capital_eth: f64,
    pub cycle_interval_secs: u64,
    pub min_apy: Option<f64>,
    pub max_il_risk: Option<String>,
    pub stop_loss_pct: f64,
    pub enabled: bool,
}
