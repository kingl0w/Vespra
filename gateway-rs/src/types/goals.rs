use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStrategy {
    Compound,
    YieldRotate,
    Snipe,
    Adaptive,
}

impl Default for GoalStrategy {
    fn default() -> Self {
        Self::Adaptive
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Pending,
    Running,
    Paused,
    Cancelled,
    Completed,
    Failed,
}

impl Default for GoalStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSpec {
    pub id: Uuid,
    pub raw_goal: String,
    pub wallet_label: String,
    /// Resolved Keymaster wallet UUID. Set at goal creation by looking up
    /// `wallet_label` against Keymaster's `/wallets` endpoint. `None` only on
    /// goals created before this field existed; the runner will fail such
    /// goals at execution time rather than guess.
    #[serde(default)]
    pub wallet_id: Option<String>,
    pub chain: String,
    #[serde(default)]
    pub capital_eth: f64,
    #[serde(default)]
    pub target_gain_pct: f64,
    #[serde(default)]
    pub stop_loss_pct: f64,
    #[serde(default)]
    pub strategy: GoalStrategy,
    #[serde(default)]
    pub status: GoalStatus,
    #[serde(default)]
    pub cycles: u32,
    #[serde(default = "default_step")]
    pub current_step: String,
    #[serde(default)]
    pub entry_eth: f64,
    #[serde(default)]
    pub current_eth: f64,
    #[serde(default)]
    pub pnl_eth: f64,
    #[serde(default)]
    pub pnl_pct: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub error: Option<String>,
}

fn default_step() -> String {
    "SCOUTING".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGoalRequest {
    pub raw_goal: String,
    pub wallet_label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_spec_serialization_roundtrip() {
        let spec = GoalSpec {
            id: Uuid::new_v4(),
            raw_goal: "Grow 0.05 ETH on base_sepolia".to_string(),
            wallet_label: "base-test-1".to_string(),
            wallet_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
            chain: "base_sepolia".to_string(),
            capital_eth: 0.05,
            target_gain_pct: 10.0,
            stop_loss_pct: 5.0,
            strategy: GoalStrategy::Compound,
            status: GoalStatus::Pending,
            cycles: 0,
            current_step: "SCOUTING".to_string(),
            entry_eth: 0.05,
            current_eth: 0.05,
            pnl_eth: 0.0,
            pnl_pct: 0.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            error: None,
        };

        let json = serde_json::to_string(&spec).expect("serialize");
        let deserialized: GoalSpec = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.id, spec.id);
        assert_eq!(deserialized.raw_goal, spec.raw_goal);
        assert_eq!(deserialized.capital_eth, spec.capital_eth);
        assert_eq!(deserialized.strategy, GoalStrategy::Compound);
        assert_eq!(deserialized.status, GoalStatus::Pending);
        assert_eq!(deserialized.chain, "base_sepolia");
    }

    #[test]
    fn goal_strategy_defaults_to_adaptive() {
        let json = r#"{"strategy": null}"#;
        // When missing, default kicks in
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            strategy: GoalStrategy,
        }
        let w: Wrap = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(w.strategy, GoalStrategy::Adaptive);
    }

    #[test]
    fn goal_status_serde_snake_case() {
        let json = serde_json::to_string(&GoalStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
        let parsed: GoalStatus = serde_json::from_str("\"cancelled\"").unwrap();
        assert_eq!(parsed, GoalStatus::Cancelled);
    }
}
