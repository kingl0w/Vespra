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

// ─── Yield decisions ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum YieldDecision {
    Rebalance {
        target_protocol: String,
        target_pool_id: String,
        expected_gain_pct: f64,
        reasoning: String,
    },
    Hold { reasoning: String },
}

// ─── Sniper decisions ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum SniperDecision {
    Enter {
        confidence: f64,
        max_entry_eth: f64,
        reasoning: String,
    },
    Pass { reasoning: String },
}

// ─── Coordinator decisions ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandIntent {
    pub strategy: String,
    pub wallet_id: Option<String>,
    pub capital_eth: Option<f64>,
    pub chain: Option<String>,
    pub max_eth: Option<f64>,
    pub stop_loss_pct: Option<f64>,
    pub threshold_pct: Option<f64>,
    pub reasoning: String,
}

// ─── Launcher decisions ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum LaunchDecision {
    Approved {
        suggested_liquidity_eth: f64,
        reasoning: String,
    },
    Rejected { reasoning: String },
}

// ─── Executor ─────────────────────────────────────────────────

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
