#!/usr/bin/env python3
"""Vespra API Proxy — thin reverse proxy for the dashboard SPA.

Routes /api/* to internal services on localhost. Zero dependencies.
Designed to sit behind a Cloudflare Tunnel with Cloudflare Access.
"""

import json, os, sys
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.request import Request, urlopen
from urllib.error import URLError, HTTPError

HOST = os.environ.get("VESPRA_PROXY_HOST", "127.0.0.1")
PORT = int(os.environ.get("VESPRA_PROXY_PORT", "9200"))
KEYMASTER_TOKEN = os.environ.get("VESPRA_KM_AUTH_TOKEN", "")
ALLOWED_ORIGIN = os.environ.get("VESPRA_CORS_ORIGIN", "*")

# CF Tunnel header — when set, only allow requests with this header
CF_ACCESS_REQUIRED = os.environ.get("VESPRA_CF_ACCESS_REQUIRED", "").lower() == "true"

# ─── Route table ──────────────────────────────────────────────────
# /api/<prefix>/[rest] → http://localhost:<port>/<target_prefix>/[rest]

import threading, time as _time

# ─── Rate limiting config (token bucket, per IP) ──────────────────
RL_AGENT_RPM      = int(os.environ.get("RL_AGENT_RPM",      "10"))   # /api/agent/*  per min
RL_WALLET_RPH     = int(os.environ.get("RL_WALLET_RPH",     "5"))    # wallet create per hour
RL_TX_RPH         = int(os.environ.get("RL_TX_RPH",         "20"))   # tx send/dispatch per hour

# Paths that map to each limit bucket
_RL_AGENT_PREFIXES  = ("/api/agent", "/api/swarm")
_RL_WALLET_PREFIXES = ("/api/wallet",)    # POST only
_RL_TX_PREFIXES     = ("/api/tx", "/api/dispatch")  # POST only


class _TokenBucket:
    """Thread-safe token bucket for a single IP+bucket key.

    capacity  — max tokens (burst size = limit value)
    refill_rate — tokens per second
    """
    __slots__ = ("_lock", "_tokens", "_last", "_capacity", "_rate")

    def __init__(self, capacity: float, refill_rate: float):
        self._lock     = threading.Lock()
        self._tokens   = float(capacity)
        self._last     = _time.monotonic()
        self._capacity = capacity
        self._rate     = refill_rate  # tokens/sec

    def consume(self) -> tuple[bool, float]:
        """Try to consume one token. Returns (allowed, retry_after_seconds)."""
        with self._lock:
            now = _time.monotonic()
            elapsed = now - self._last
            self._last = now
            self._tokens = min(self._capacity, self._tokens + elapsed * self._rate)
            if self._tokens >= 1.0:
                self._tokens -= 1.0
                return True, 0.0
            # Seconds until next token available
            wait = (1.0 - self._tokens) / self._rate
            return False, wait


class RateLimiter:
    """Per-IP token bucket registry with three named buckets."""

    def __init__(self):
        self._lock    = threading.Lock()
        self._buckets: dict[str, _TokenBucket] = {}
        # Bucket configs: (capacity, refill_rate_per_sec)
        self._configs = {
            "agent":  (float(RL_AGENT_RPM),  RL_AGENT_RPM  / 60.0),
            "wallet": (float(RL_WALLET_RPH), RL_WALLET_RPH / 3600.0),
            "tx":     (float(RL_TX_RPH),     RL_TX_RPH     / 3600.0),
        }

    def _key(self, ip: str, bucket: str) -> str:
        return f"{ip}:{bucket}"

    def _get_bucket(self, ip: str, bucket: str) -> _TokenBucket:
        key = self._key(ip, bucket)
        with self._lock:
            if key not in self._buckets:
                cap, rate = self._configs[bucket]
                self._buckets[key] = _TokenBucket(cap, rate)
            return self._buckets[key]

    def check(self, ip: str, bucket: str) -> tuple[bool, float]:
        """Returns (allowed, retry_after_seconds)."""
        return self._get_bucket(ip, bucket).consume()

    def classify(self, method: str, path: str) -> str | None:
        """Return bucket name for this request, or None if not rate-limited."""
        if any(path.startswith(p) for p in _RL_AGENT_PREFIXES):
            return "agent"
        if method == "POST":
            if any(path.startswith(p) for p in _RL_WALLET_PREFIXES):
                return "wallet"
            if any(path.startswith(p) for p in _RL_TX_PREFIXES):
                return "tx"
        return None


# Module-level singleton
_rate_limiter = RateLimiter()


def _get_client_ip(headers) -> str:
    """Extract real client IP, preferring Cloudflare headers."""
    # CF-Connecting-IP is set by Cloudflare Tunnel and is trustworthy
    cf_ip = headers.get("Cf-Connecting-Ip") or headers.get("X-Real-Ip")
    if cf_ip:
        return cf_ip.strip()
    return "unknown"


ROUTES = {
    # Health aggregation
    "health/gateway":   ("localhost:9000",  "/health"),
    "health/boiler":    ("localhost:9090",  "/health"),
    "health/keymaster": ("localhost:9100",  "/health"),

    # Gateway agent endpoints
    "agent":            ("localhost:9000",  "/agent"),
    "swarm":            ("localhost:9000",  "/swarm"),

    # NullBoiler DAG endpoints
    "dag":              ("localhost:9090",  "/runs"),

    # Keymaster endpoints
    "wallet":           ("localhost:9100",  "/wallets"),
    "balance":          ("localhost:9100",  "/balance"),
    "balances":         ("localhost:9100",  "/balances"),
    "chain":            ("localhost:9100",  "/chain"),
    "tx":               ("localhost:9100",  "/tx"),
    "dispatch":         ("localhost:9100",  "/dispatch"),
    "settings":         ("localhost:9100",  "/settings"),
}

# Write methods that need auth token forwarded
WRITE_METHODS = {"POST", "PUT", "DELETE", "PATCH"}


def resolve_route(path):
    """Map /api/X/Y/Z to (upstream_host, upstream_path).

    Returns (host, path) or (None, None) if no match.
    """
    if not path.startswith("/api/"):
        return None, None

    rest = path[5:]  # strip /api/

    # Try exact prefix matches, longest first
    for prefix in sorted(ROUTES.keys(), key=len, reverse=True):
        if rest == prefix or rest.startswith(prefix + "/"):
            host, target = ROUTES[prefix]
            remainder = rest[len(prefix):]  # could be "" or "/something"
            return host, target + remainder

    return None, None


class ProxyHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write(f"[proxy] {args[0]} {args[1]} {args[2]}\n")

    def _cors_headers(self):
        return {
            "Access-Control-Allow-Origin": ALLOWED_ORIGIN,
            "Access-Control-Allow-Methods": "GET, POST, PUT, DELETE, OPTIONS",
            "Access-Control-Allow-Headers": "Content-Type, Authorization, X-Request-ID",
            "Access-Control-Max-Age": "86400",
        }

    def _send_json(self, code, data):
        body = json.dumps(data).encode()
        self.send_response(code)
        for k, v in self._cors_headers().items():
            self.send_header(k, v)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _check_cf_access(self):
        """If CF Access enforcement is on, reject requests without the header."""
        if not CF_ACCESS_REQUIRED:
            return True
        cf_header = self.headers.get("Cf-Access-Authenticated-User-Email")
        if not cf_header:
            self._send_json(403, {"error": "Cloudflare Access required"})
            return False
        return True

    def _proxy(self, method):
        if not self._check_cf_access():
            return

        # ── Rate limiting ─────────────────────────────────────────
        bucket = _rate_limiter.classify(method, self.path)
        if bucket:
            client_ip = _get_client_ip(self.headers)
            allowed, retry_after = _rate_limiter.check(client_ip, bucket)
            if not allowed:
                retry_ceil = int(retry_after) + 1
                sys.stderr.write(
                    f"[proxy] RATE_LIMIT ip={client_ip} bucket={bucket} "
                    f"path={self.path} retry_after={retry_ceil}s\n"
                )
                body = json.dumps({
                    "error":       "rate limit exceeded",
                    "bucket":      bucket,
                    "retry_after": retry_ceil,
                }).encode()
                self.send_response(429)
                for k, v in self._cors_headers().items():
                    self.send_header(k, v)
                self.send_header("Content-Type",   "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Retry-After",    str(retry_ceil))
                self.end_headers()
                self.wfile.write(body)
                return

        host, upstream_path = resolve_route(self.path)

        if not host:
            if self.path == "/api/health":
                return self._health_aggregate()
            if self.path == "/api/rate-limits":
                return self._send_json(200, {
                    "limits": {
                        "agent":  {"max": RL_AGENT_RPM,  "window": "1m", "paths": list(_RL_AGENT_PREFIXES)},
                        "wallet": {"max": RL_WALLET_RPH, "window": "1h", "paths": list(_RL_WALLET_PREFIXES)},
                        "tx":     {"max": RL_TX_RPH,     "window": "1h", "paths": list(_RL_TX_PREFIXES)},
                    }
                })
            self._send_json(404, {"error": "not found"})
            return

        # Read request body
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length > 0 else None

        # Build upstream request
        url = f"http://{host}{upstream_path}"
        headers = {"Content-Type": self.headers.get("Content-Type", "application/json")}

        # Forward auth token on all requests to Keymaster
        if KEYMASTER_TOKEN and "9100" in host:
            headers["Authorization"] = f"Bearer {KEYMASTER_TOKEN}"

        # Forward auth token on write requests to gateway too
        if method in WRITE_METHODS and KEYMASTER_TOKEN and "9000" in host:
            # Gateway doesn't need auth but forward request ID if present
            pass

        try:
            req = Request(url, data=body, headers=headers, method=method)
            with urlopen(req, timeout=120) as resp:
                resp_body = resp.read()
                self.send_response(resp.status)
                for k, v in self._cors_headers().items():
                    self.send_header(k, v)
                self.send_header("Content-Type", resp.headers.get("Content-Type", "application/json"))
                self.send_header("Content-Length", str(len(resp_body)))
                self.end_headers()
                self.wfile.write(resp_body)
        except HTTPError as e:
            resp_body = e.read()
            ct = e.headers.get("Content-Type", "application/json")
            # If upstream returned non-JSON, wrap it in JSON for the frontend
            if "json" not in ct:
                resp_body = json.dumps({"error": resp_body.decode("utf-8", errors="replace")}).encode()
                ct = "application/json"
            self.send_response(e.code)
            for k, v in self._cors_headers().items():
                self.send_header(k, v)
            self.send_header("Content-Type", ct)
            self.send_header("Content-Length", str(len(resp_body)))
            self.end_headers()
            self.wfile.write(resp_body)
        except URLError as e:
            self._send_json(502, {"error": f"upstream unreachable: {e.reason}"})
        except Exception as e:
            self._send_json(500, {"error": str(e)})

    def _health_aggregate(self):
        """GET /api/health — aggregate health from all 3 services."""
        services = {
            "gateway":   "http://localhost:9000/health",
            "boiler":    "http://localhost:9090/health",
            "keymaster": "http://localhost:9100/health",
        }
        results = {}
        all_ok = True
        for name, url in services.items():
            try:
                req = Request(url, method="GET")
                with urlopen(req, timeout=5) as resp:
                    data = json.loads(resp.read())
                    results[name] = {"status": "ok", "data": data}
            except Exception as e:
                results[name] = {"status": "down", "error": str(e)}
                all_ok = False

        self._send_json(200, {
            "status": "ok" if all_ok else "degraded",
            "services": results,
        })

    def do_OPTIONS(self):
        self.send_response(204)
        for k, v in self._cors_headers().items():
            self.send_header(k, v)
        self.end_headers()

    def do_GET(self):
        self._proxy("GET")

    def do_POST(self):
        self._proxy("POST")

    def do_PUT(self):
        self._proxy("PUT")

    def do_DELETE(self):
        self._proxy("DELETE")


if __name__ == "__main__":
    HTTPServer.allow_reuse_address = True
    server = HTTPServer((HOST, PORT), ProxyHandler)
    print(f"Vespra API Proxy on {HOST}:{PORT}", flush=True)
    print(f"  CORS origin: {ALLOWED_ORIGIN}", flush=True)
    print(f"  CF Access required: {CF_ACCESS_REQUIRED}", flush=True)
    print(f"  Keymaster token: {'set' if KEYMASTER_TOKEN else 'NOT SET'}", flush=True)
    print(f"  Rate limits: agent={RL_AGENT_RPM}/min  wallet={RL_WALLET_RPH}/hr  tx={RL_TX_RPH}/hr", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.server_close()
