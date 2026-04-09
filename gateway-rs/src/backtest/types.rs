use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestRequest {
    pub raw_goal: String,
    pub wallet_label: String,
    pub chain: String,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    #[serde(default)]
    pub mode: BacktestMode,
    ///defillama pool id used for the apy series. optional — defaults to a
    ///stablecoin pool if omitted, but real backtests should always set it.
    #[serde(default)]
    pub pool_id: Option<String>,
    ///coingecko coin id used for the price series.
    #[serde(default)]
    pub coingecko_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BacktestMode {
    #[default]
    Rules,
    Agents,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub date: NaiveDate,
    pub value_eth: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub mode: BacktestMode,
    pub strategy_summary: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    pub pnl_pct: f64,
    pub pnl_eth: f64,
    pub max_drawdown_pct: f64,
    pub win_rate_pct: f64,
    pub total_trades: u32,
    pub fee_estimate_eth: f64,
    pub equity_curve: Vec<EquityPoint>,
}

///lightweight projection used by the index endpoint so the dashboard can
///list past runs without paying the bandwidth cost of the full equity curve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestSummary {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub strategy_summary: String,
    pub pnl_pct: f64,
    pub mode: BacktestMode,
}

impl From<&BacktestResult> for BacktestSummary {
    fn from(r: &BacktestResult) -> Self {
        Self {
            id: r.id,
            created_at: r.created_at,
            strategy_summary: r.strategy_summary.clone(),
            pnl_pct: r.pnl_pct,
            mode: r.mode,
        }
    }
}
