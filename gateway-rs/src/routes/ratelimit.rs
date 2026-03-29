use std::time::Instant;

use dashmap::DashMap;

/// Token bucket for a single IP.
struct TokenBucket {
    tokens: f64,
    last: Instant,
    capacity: f64,
    refill_rate: f64, // tokens per second
}

impl TokenBucket {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            tokens: capacity,
            last: Instant::now(),
            capacity,
            refill_rate,
        }
    }

    /// Try to consume one token. Returns (allowed, retry_after_seconds).
    fn consume(&mut self) -> (bool, f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            (true, 0.0)
        } else {
            let wait = (1.0 - self.tokens) / self.refill_rate;
            (false, wait)
        }
    }
}

/// Rate limiter for the Alchemy webhook endpoint only.
/// Per-IP token bucket, configurable max requests per minute.
pub struct WebhookRateLimiter {
    buckets: DashMap<String, TokenBucket>,
    capacity: f64,
    refill_rate: f64,
}

impl WebhookRateLimiter {
    pub fn new(max_rpm: u64) -> Self {
        let capacity = max_rpm as f64;
        Self {
            buckets: DashMap::new(),
            capacity,
            refill_rate: capacity / 60.0,
        }
    }

    /// Check rate limit for the given IP. Returns (allowed, retry_after_seconds).
    pub fn check(&self, ip: &str) -> (bool, f64) {
        let capacity = self.capacity;
        let refill_rate = self.refill_rate;
        let mut entry = self
            .buckets
            .entry(ip.to_string())
            .or_insert_with(|| TokenBucket::new(capacity, refill_rate));
        entry.consume()
    }

    /// Return rate limit config as JSON for /api/rate-limits endpoint.
    pub fn config_json(&self) -> serde_json::Value {
        serde_json::json!({
            "webhook": {
                "max": self.capacity as u64,
                "window": "1m",
                "path": "/webhooks/alchemy"
            }
        })
    }
}

/// Extract client IP from Cloudflare headers, falling back to socket addr.
pub fn extract_client_ip(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string()
}
