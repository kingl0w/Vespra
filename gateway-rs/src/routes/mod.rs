pub mod config;
pub mod fees;
pub mod health;
pub mod swarm;
pub mod trade_up;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::Router;

use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;
use crate::orchestrator::trade_up::TradeUpOrchestrator;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<GatewayConfig>,
    pub chain_registry: Arc<ChainRegistry>,
    pub redis: Arc<redis::Client>,
    pub trade_up_orchestrator: Arc<TradeUpOrchestrator>,
    pub kill_flag: Arc<AtomicBool>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(health::router())
        .merge(trade_up::router())
        .merge(swarm::router())
        .merge(config::router())
        .merge(fees::router())
        .with_state(state)
}
