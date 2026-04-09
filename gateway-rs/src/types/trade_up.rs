use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradePosition {
    pub id: String,
    pub wallet: String,
    pub chain: String,
    pub token_address: String,
    pub token_symbol: String,
    pub entry_price_usd: f64,
    pub entry_eth: f64,
    pub token_amount: f64,
    pub opened_at: i64,
    pub status: PositionStatus,
    pub exit_price_usd: Option<f64>,
    pub exit_eth: Option<f64>,
    pub gas_cost_eth: Option<f64>,
    pub net_gain_eth: Option<f64>,
    pub exit_reason: Option<String>,
    pub closed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PositionStatus {
    Open,
    Exiting,
    Closed,
    Failed,
}

///redis key for the full position history array.
pub const REDIS_TRADE_POSITIONS: &str = "vespra:trade_positions";

///redis key for the currently active position id.
pub const REDIS_ACTIVE_POSITION: &str = "vespra:trade_up:active_position";
