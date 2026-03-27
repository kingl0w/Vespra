pub mod config;
pub mod fees;
pub mod health;
pub mod proxy;
pub mod ratelimit;
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
use crate::orchestrator::trade_up::TradeUpOrchestrator;

use self::ratelimit::RateLimiter;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<GatewayConfig>,
    pub chain_registry: Arc<ChainRegistry>,
    pub redis: Arc<redis::Client>,
    pub trade_up_orchestrator: Arc<TradeUpOrchestrator>,
    pub kill_flag: Arc<AtomicBool>,
    pub rate_limiter: Arc<RateLimiter>,
}

/// Middleware: Cloudflare Access check
async fn cf_access_middleware(
    request: Request<Body>,
    next: Next,
) -> Response {
    // Extract config from extensions — we check the flag inline
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

/// Middleware: rate limiting on /api/* paths
async fn rate_limit_middleware(
    request: Request<Body>,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();

    let rate_limiter = request
        .extensions()
        .get::<Arc<RateLimiter>>()
        .cloned();

    if let Some(limiter) = rate_limiter {
        if let Some(bucket) = limiter.classify(&method, &path) {
            let client_ip = ratelimit::extract_client_ip(request.headers());
            let (allowed, retry_after) = limiter.check(&client_ip, bucket);

            if !allowed {
                let retry_ceil = retry_after.ceil() as u64;
                tracing::warn!(
                    "RATE_LIMIT ip={client_ip} bucket={bucket} path={path} retry_after={retry_ceil}s"
                );
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", retry_ceil.to_string())],
                    axum::Json(serde_json::json!({
                        "error": "rate limit exceeded",
                        "bucket": bucket,
                        "retry_after": retry_ceil,
                    })),
                )
                    .into_response();
            }
        }
    }

    next.run(request).await
}

/// Middleware: inject shared state into request extensions for other middleware
async fn inject_extensions(
    axum::extract::State(state): axum::extract::State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    request.extensions_mut().insert(state.config.clone());
    request
        .extensions_mut()
        .insert(state.rate_limiter.clone());
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
        // Existing gateway routes
        .merge(health::router())
        .merge(trade_up::router())
        .merge(swarm::router())
        .merge(config::router())
        .merge(fees::router())
        .merge(yield_routes::router())
        // Proxy routes — nested under /api, won't interfere with top-level routes
        .merge(proxy::router())
        .with_state(state.clone())
        // Middleware stack (outermost first)
        .layer(middleware::from_fn_with_state(state.clone(), inject_extensions))
        .layer(middleware::from_fn(rate_limit_middleware))
        .layer(middleware::from_fn(cf_access_middleware))
        .layer(cors)
}
