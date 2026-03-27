use serde::{Deserialize, Serialize};
use super::opportunity::Opportunity;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum ScoutDecision {
    Opportunities(Vec<Opportunity>),
    NoOpportunities { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum RiskScore {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum RiskDecision {
    GatePass { score: RiskScore },
    GateBlock { score: RiskScore, reason: String },
}

impl RiskDecision {
    pub fn is_blocked(&self) -> bool {
        matches!(self, RiskDecision::GateBlock { .. })
    }

    pub fn score(&self) -> &RiskScore {
        match self {
            RiskDecision::GatePass { score } => score,
            RiskDecision::GateBlock { score, .. } => score,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum TraderDecision {
    Swap {
        token_in: String,
        token_out: String,
        amount_in_wei: String,
        expected_gain_pct: f64,
        reasoning: String,
    },
    Hold { reason: String },
    Exit { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum SentinelDecision {
    Healthy,
    StopLoss { wallet_id: uuid::Uuid, loss_pct: f64 },
    Alert { message: String },
}

impl SentinelDecision {
    pub fn is_stop_loss(&self) -> bool {
        matches!(self, SentinelDecision::StopLoss { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorResult {
    pub status: ExecutorStatus,
    pub tx_hash: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum ExecutorStatus {
    Success,
    Failed,
    Simulated,
}
