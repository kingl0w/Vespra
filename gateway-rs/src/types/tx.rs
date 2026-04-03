use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TxStatus {
    Confirmed {
        tx_hash: String,
        block_number: u64,
        gas_used: u64,
    },
    Reverted {
        tx_hash: String,
        block_number: u64,
        gas_used: u64,
    },
    Timeout {
        tx_hash: String,
        attempts: u32,
    },
    DryRun {
        calldata: serde_json::Value,
    },
    Failed {
        error: String,
    },
}

impl TxStatus {
    pub fn is_confirmed(&self) -> bool {
        matches!(self, TxStatus::Confirmed { .. })
    }

    pub fn tx_hash(&self) -> Option<&str> {
        match self {
            TxStatus::Confirmed { tx_hash, .. }
            | TxStatus::Reverted { tx_hash, .. }
            | TxStatus::Timeout { tx_hash, .. } => Some(tx_hash),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_status_serde_roundtrip() {
        let confirmed = TxStatus::Confirmed {
            tx_hash: "0xabc".into(),
            block_number: 12345,
            gas_used: 21000,
        };
        let json = serde_json::to_string(&confirmed).unwrap();
        assert!(json.contains("\"status\":\"confirmed\""));
        let parsed: TxStatus = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_confirmed());
        assert_eq!(parsed.tx_hash(), Some("0xabc"));

        let dry = TxStatus::DryRun {
            calldata: serde_json::json!({"to": "0x1"}),
        };
        let json = serde_json::to_string(&dry).unwrap();
        assert!(json.contains("\"status\":\"dry_run\""));

        let reverted = TxStatus::Reverted {
            tx_hash: "0xdef".into(),
            block_number: 100,
            gas_used: 50000,
        };
        assert!(!reverted.is_confirmed());
        assert_eq!(reverted.tx_hash(), Some("0xdef"));
    }
}
