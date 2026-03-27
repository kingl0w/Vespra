use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WalletState {
    pub wallet_id: uuid::Uuid,
    pub address: String,
    pub chain: String,
    pub balance_eth: f64,
    pub token_positions: Vec<TokenPosition>,
    pub alerts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenPosition {
    pub symbol: String,
    pub balance: f64,
    pub value_usd: f64,
    pub pnl_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceData {
    pub price_usd: f64,
    pub price_change_24h_pct: f64,
    pub source: String,
    pub timestamp: i64,
}

impl Default for PriceData {
    fn default() -> Self {
        Self {
            price_usd: 0.0,
            price_change_24h_pct: 0.0,
            source: "none".into(),
            timestamp: 0,
        }
    }
}
