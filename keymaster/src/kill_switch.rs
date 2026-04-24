use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::AppError;

#[derive(Clone)]
pub struct KillSwitch {
    active: Arc<AtomicBool>,
    activated_at: Arc<RwLock<Option<DateTime<Utc>>>>,
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KillSwitchStatus {
    pub active: bool,
    pub activated_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, Deserialize)]
struct PersistedState {
    active: bool,
    activated_at: Option<DateTime<Utc>>,
}

impl KillSwitch {
    pub fn load(path: PathBuf) -> Self {
        let (active, activated_at) = match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<PersistedState>(&content) {
                Ok(s) => (s.active, s.activated_at),
                Err(e) => {
                    tracing::warn!(
                        "[kill_switch] failed to parse state file {}: {e} — starting disabled",
                        path.display()
                    );
                    (false, None)
                }
            },
            Err(_) => (false, None),
        };
        if active {
            tracing::warn!(
                "[kill_switch] loaded ACTIVE state from {} — signing disabled",
                path.display()
            );
        } else {
            tracing::info!("[kill_switch] loaded (inactive) from {}", path.display());
        }
        Self {
            active: Arc::new(AtomicBool::new(active)),
            activated_at: Arc::new(RwLock::new(activated_at)),
            path,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub async fn activate(&self) -> KillSwitchStatus {
        let now = Utc::now();
        self.active.store(true, Ordering::Relaxed);
        *self.activated_at.write().await = Some(now);
        self.persist(true, Some(now));
        tracing::warn!("[kill_switch] ACTIVATED — signing disabled");
        KillSwitchStatus {
            active: true,
            activated_at: Some(now),
        }
    }

    pub async fn deactivate(&self) -> KillSwitchStatus {
        self.active.store(false, Ordering::Relaxed);
        *self.activated_at.write().await = None;
        self.persist(false, None);
        tracing::info!("[kill_switch] DEACTIVATED — signing re-enabled");
        KillSwitchStatus {
            active: false,
            activated_at: None,
        }
    }

    pub async fn status(&self) -> KillSwitchStatus {
        KillSwitchStatus {
            active: self.is_active(),
            activated_at: *self.activated_at.read().await,
        }
    }

    pub fn check(&self) -> Result<(), AppError> {
        if self.is_active() {
            Err(AppError::KillSwitchActive)
        } else {
            Ok(())
        }
    }

    fn persist(&self, active: bool, activated_at: Option<DateTime<Utc>>) {
        let state = PersistedState {
            active,
            activated_at,
        };
        let json = match serde_json::to_string(&state) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("[kill_switch] failed to serialize state: {e}");
                return;
            }
        };
        if let Some(parent) = self.path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::error!(
                    "[kill_switch] failed to create state dir {}: {e}",
                    parent.display()
                );
                return;
            }
        }
        if let Err(e) = std::fs::write(&self.path, json) {
            tracing::error!(
                "[kill_switch] failed to persist state to {}: {e}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path() -> PathBuf {
        let name = format!("kill-switch-test-{}.state", uuid::Uuid::new_v4());
        std::env::temp_dir().join(name)
    }

    #[tokio::test]
    async fn activate_sets_flag_and_timestamp() {
        let path = temp_path();
        let ks = KillSwitch::load(path.clone());
        assert!(!ks.is_active());
        let status = ks.activate().await;
        assert!(status.active);
        assert!(status.activated_at.is_some());
        assert!(ks.is_active());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn deactivate_clears_flag() {
        let path = temp_path();
        let ks = KillSwitch::load(path.clone());
        ks.activate().await;
        let status = ks.deactivate().await;
        assert!(!status.active);
        assert!(status.activated_at.is_none());
        assert!(!ks.is_active());
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn status_reflects_activate_and_deactivate() {
        let path = temp_path();
        let ks = KillSwitch::load(path.clone());

        let s0 = ks.status().await;
        assert!(!s0.active);
        assert!(s0.activated_at.is_none());

        let s1 = ks.activate().await;
        let s1_read = ks.status().await;
        assert!(s1_read.active);
        assert_eq!(s1, s1_read);

        let _s2 = ks.deactivate().await;
        let s2_read = ks.status().await;
        assert!(!s2_read.active);
        assert!(s2_read.activated_at.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn state_persists_across_restart() {
        let path = temp_path();
        {
            let ks = KillSwitch::load(path.clone());
            ks.activate().await;
            assert!(ks.is_active());
        }
        //simulate restart — load from same path
        let ks2 = KillSwitch::load(path.clone());
        assert!(ks2.is_active(), "active flag must survive restart");
        let status = ks2.status().await;
        assert!(status.active);
        assert!(status.activated_at.is_some(), "timestamp must survive restart");

        //clean deactivate also persists
        ks2.deactivate().await;
        let ks3 = KillSwitch::load(path.clone());
        assert!(!ks3.is_active(), "inactive flag must survive restart after deactivate");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn check_returns_err_when_active() {
        let path = temp_path();
        let ks = KillSwitch::load(path.clone());
        assert!(ks.check().is_ok());
        ks.activate().await;
        let err = ks.check().expect_err("expected error when active");
        assert!(matches!(err, AppError::KillSwitchActive));
        let _ = std::fs::remove_file(&path);
    }
}
