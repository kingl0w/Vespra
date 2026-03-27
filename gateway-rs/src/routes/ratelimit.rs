use std::time::Instant;

use dashmap::DashMap;

/// Token bucket for a single IP+bucket key.
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

#[derive(Clone, Copy)]
pub struct BucketConfig {
    pub capacity: f64,
    pub refill_rate: f64,
}

pub struct RateLimiter {
    buckets: DashMap<String, TokenBucket>,
    configs: std::collections::HashMap<String, BucketConfig>,
}

impl RateLimiter {
    pub fn new(agent_rpm: u32, wallet_rph: u32, tx_rph: u32) -> Self {
        let mut configs = std::collections::HashMap::new();
        configs.insert(
            "agent".into(),
            BucketConfig {
                capacity: agent_rpm as f64,
                refill_rate: agent_rpm as f64 / 60.0,
            },
        );
        configs.insert(
            "wallet".into(),
            BucketConfig {
                capacity: wallet_rph as f64,
                refill_rate: wallet_rph as f64 / 3600.0,
            },
        );
        configs.insert(
            "tx".into(),
            BucketConfig {
                capacity: tx_rph as f64,
                refill_rate: tx_rph as f64 / 3600.0,
            },
        );
        Self {
            buckets: DashMap::new(),
            configs,
        }
    }

    /// Classify a request into a rate limit bucket. Returns None if not limited.
    pub fn classify(&self, method: &str, path: &str) -> Option<&str> {
        if path.starts_with("/api/agent") || path.starts_with("/api/swarm") {
            return Some("agent");
        }
        if method == "POST" {
            if path.starts_with("/api/wallet") {
                return Some("wallet");
            }
            if path.starts_with("/api/tx") || path.starts_with("/api/dispatch") {
                return Some("tx");
            }
        }
        None
    }

    /// Check rate limit. Returns (allowed, retry_after_seconds).
    pub fn check(&self, ip: &str, bucket_name: &str) -> (bool, f64) {
        let key = format!("{ip}:{bucket_name}");
        let config = match self.configs.get(bucket_name) {
            Some(c) => *c,
            None => return (true, 0.0),
        };
        let mut entry = self.buckets.entry(key).or_insert_with(|| {
            TokenBucket::new(config.capacity, config.refill_rate)
        });
        entry.consume()
    }

    /// Return rate limit config as JSON for /api/rate-limits endpoint.
    pub fn config_json(&self) -> serde_json::Value {
        serde_json::json!({
            "limits": {
                "agent": {
                    "max": self.configs.get("agent").map(|c| c.capacity as u32).unwrap_or(0),
                    "window": "1m",
                    "paths": ["/api/agent", "/api/swarm"]
                },
                "wallet": {
                    "max": self.configs.get("wallet").map(|c| c.capacity as u32).unwrap_or(0),
                    "window": "1h",
                    "paths": ["/api/wallet"]
                },
                "tx": {
                    "max": self.configs.get("tx").map(|c| c.capacity as u32).unwrap_or(0),
                    "window": "1h",
                    "paths": ["/api/tx", "/api/dispatch"]
                }
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
