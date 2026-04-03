/// Placeholder for VES-85 (Yield Scheduler). Provides the type expected by AppState.

pub type SharedSchedulerStatus = std::sync::Arc<tokio::sync::RwLock<SchedulerStatus>>;

#[derive(Debug, Clone, Default)]
pub struct SchedulerStatus {
    pub running: bool,
}

pub fn default_status() -> SharedSchedulerStatus {
    std::sync::Arc::new(tokio::sync::RwLock::new(SchedulerStatus::default()))
}
