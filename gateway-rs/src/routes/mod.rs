pub mod config;
pub mod fees;
pub mod health;
pub mod launcher;
pub mod portfolio;
pub mod proxy;
pub mod ratelimit;
pub mod sniper;
pub mod swarm;
pub mod trade_up;
pub mod yield_routes;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::chain::ChainRegistry;
use crate::config::GatewayConfig;
use crate::orchestrator::command::CommandOrchestrator;
use crate::orchestrator::launcher::LauncherOrchestrator;
use crate::orchestrator::portfolio::PortfolioOrchestrator;
use crate::orchestrator::sniper::SniperOrchestrator;
use crate::orchestrator::trade_up::TradeUpOrchestrator;
use crate::orchestrator::yield_rot::YieldOrchestrator;

use self::ratelimit::WebhookRateLimiter;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<GatewayConfig>,
    pub chain_registry: Arc<ChainRegistry>,
    pub redis: Arc<redis::Client>,
    pub trade_up_orchestrator: Arc<TradeUpOrchestrator>,
    pub yield_orchestrator: Arc<YieldOrchestrator>,
    pub sniper_orchestrator: Arc<SniperOrchestrator>,
    pub command_orchestrator: Arc<CommandOrchestrator>,
    pub launcher_orchestrator: Arc<LauncherOrchestrator>,
    pub portfolio_orchestrator: Arc<PortfolioOrchestrator>,
    pub kill_flag: Arc<AtomicBool>,
    pub webhook_rate_limiter: Arc<WebhookRateLimiter>,
}

/// Middleware: Cloudflare Access check
async fn cf_access_middleware(
    request: Request<Body>,
    next: Next,
) -> Response {
    let cf_required = request
        .extensions()
        .get::<Arc<GatewayConfig>>()
        .map(|c| c.cf_access_required)
        .unwrap_or(false);

    if cf_required {
        let has_cf_header = request
            .headers()
            .get("cf-access-authenticated-user-email")
            .is_some();
        if !has_cf_header {
            return (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"error": "Cloudflare Access required"})),
            )
                .into_response();
        }
    }

    next.run(request).await
}

/// Middleware: inject config into request extensions for cf_access_middleware
async fn inject_extensions(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    request.extensions_mut().insert(state.config.clone());
    next.run(request).await
}

pub fn router(state: AppState) -> Router {
    // CORS layer
    let cors = if state.config.cors_origin == "*" {
        CorsLayer::new()
            .allow_origin(AllowOrigin::any())
            .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS, Method::PATCH])
            .allow_headers(tower_http::cors::Any)
            .max_age(std::time::Duration::from_secs(86400))
    } else {
        let origin: HeaderValue = state.config.cors_origin.parse().unwrap_or_else(|_| HeaderValue::from_static("*"));
        CorsLayer::new()
            .allow_origin(AllowOrigin::exact(origin))
            .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS, Method::PATCH])
            .allow_headers(tower_http::cors::Any)
            .max_age(std::time::Duration::from_secs(86400))
    };

    Router::new()
        .merge(health::router())
        .merge(trade_up::router())
        .merge(swarm::router())
        .merge(config::router())
        .merge(fees::router())
        .merge(yield_routes::router())
        .merge(sniper::router())
        .merge(launcher::router())
        .merge(portfolio::router())
        .merge(proxy::router())
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state.clone(), inject_extensions))
        .layer(middleware::from_fn(cf_access_middleware))
        .layer(cors)
}
