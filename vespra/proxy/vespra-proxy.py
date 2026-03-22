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

        host, upstream_path = resolve_route(self.path)

        if not host:
            # /api/health with no suffix → aggregate all health checks
            if self.path == "/api/health":
                return self._health_aggregate()
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
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.server_close()
