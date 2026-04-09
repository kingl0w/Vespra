use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;

use crate::config::GatewayConfig;

//── token bucket ─────────────────────────────────────────────────

struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    fn new(capacity: f64) -> Self {
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
        }
    }

    ///refill tokens based on elapsed time, then try to consume one.
    ///returns (allowed, retry_after_seconds).
    fn try_consume(&mut self, capacity: f64, refill_rate: f64) -> (bool, f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * refill_rate).min(capacity);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            (true, 0.0)
        } else {
            let wait = (1.0 - self.tokens) / refill_rate;
            (false, wait)
        }
    }
}

//── rate limiter ─────────────────────────────────────────────────

#[derive(Clone)]
pub struct RateLimiter {
    buckets: Arc<DashMap<IpAddr, TokenBucket>>,
    max_tokens: u32,
    refill_rate: f64,
    label: &'static str,
}

impl RateLimiter {
    ///create a limiter with `max` requests per `window_secs`.
    ///e.g. `new(10, 60, "agent")` → 10 requests/minute.
    pub fn new(max: u32, window_secs: u32, label: &'static str) -> Self {
        Self {
            buckets: Arc::new(DashMap::new()),
            max_tokens: max,
            refill_rate: max as f64 / window_secs as f64,
            label,
        }
    }

    ///check rate limit for a given ip. returns `ok(())` or the 429 response.
    fn check(&self, ip: IpAddr, path: &str) -> Result<(), Response> {
        let max_tokens = self.max_tokens as f64;
        let refill_rate = self.refill_rate;

        let mut entry = self
            .buckets
            .entry(ip)
            .or_insert_with(|| TokenBucket::new(max_tokens));

        let (allowed, retry_after) = entry.try_consume(max_tokens, refill_rate);

        if allowed {
            Ok(())
        } else {
            let retry_secs = retry_after.ceil() as u64;
            tracing::warn!(
                ip = %ip,
                endpoint = %path,
                limit = self.label,
                "rate_limit_hit"
            );

            let body = serde_json::json!({
                "error": "rate_limit_exceeded",
                "retry_after": retry_secs,
            });

            let mut resp = (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response();
            if let Ok(val) = HeaderValue::from_str(&retry_secs.to_string()) {
                resp.headers_mut().insert("Retry-After", val);
            }
            Err(resp)
        }
    }
}

//── route-specific limiter set ───────────────────────────────────

///holds the three route-group limiters + the enabled flag.
#[derive(Clone)]
pub struct RouteLimiters {
    pub enabled: bool,
    pub agent: RateLimiter,
    pub wallet_create: RateLimiter,
    pub tx_send: RateLimiter,
}

impl RouteLimiters {
    pub fn from_config(config: &GatewayConfig) -> Self {
        Self {
            enabled: config.rate_limit_enabled,
            agent: RateLimiter::new(config.rate_limit_agent_rpm, 60, "agent_rpm"),
            wallet_create: RateLimiter::new(config.rate_limit_wallet_create_rph, 3600, "wallet_create_rph"),
            tx_send: RateLimiter::new(config.rate_limit_tx_send_rph, 3600, "tx_send_rph"),
        }
    }

    ///return current config as json (no per-ip state exposed).
    pub fn config_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "agent_rpm": self.agent.max_tokens,
            "wallet_create_rph": self.wallet_create.max_tokens,
            "tx_send_rph": self.tx_send.max_tokens,
        })
    }
}

//── ip extraction ────────────────────────────────────────────────

fn extract_ip(req: &Request<Body>) -> IpAddr {
    //try cf/proxy headers first, then connectinfo, fallback to loopback
    req.headers()
        .get("cf-connecting-ip")
        .or_else(|| req.headers().get("x-real-ip"))
        .or_else(|| req.headers().get("x-forwarded-for"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            //x-forwarded-for may contain comma-separated ips; take the first
            s.split(',').next().unwrap_or(s).trim().parse::<IpAddr>().ok()
        })
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
}

//── axum middleware function ─────────────────────────────────────

pub async fn rate_limit_middleware(
    axum::extract::State(limiters): axum::extract::State<RouteLimiters>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !limiters.enabled {
        return next.run(req).await;
    }

    let ip = extract_ip(&req);
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    //determine which limiter applies (if any)
    let limiter = classify_route(&path, &method, &limiters);

    if let Some(limiter) = limiter {
        if let Err(resp) = limiter.check(ip, &path) {
            return resp;
        }
    }

    next.run(req).await
}

fn classify_route<'a>(
    path: &str,
    method: &axum::http::Method,
    limiters: &'a RouteLimiters,
) -> Option<&'a RateLimiter> {
    //agent-tier: swarm commands
    if path == "/swarm/command" || path.starts_with("/swarm/command/") {
        return Some(&limiters.agent);
    }

    //wallet creation: post /api/wallet (proxy creates wallet)
    if method == axum::http::Method::POST && (path == "/api/wallet" || path == "/api/wallet/") {
        return Some(&limiters.wallet_create);
    }

    //tx-tier: dispatch, trade-up position start
    if method == axum::http::Method::POST {
        if path == "/api/dispatch" || path.starts_with("/api/dispatch/") {
            return Some(&limiters.tx_send);
        }
        if path == "/trade-up/position/start" {
            return Some(&limiters.tx_send);
        }
    }

    None
}
