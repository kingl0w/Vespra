use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;

use super::AppState;

/// Route table: /api/{prefix}/... → (upstream_base, target_path_prefix)
/// Keymaster routes get Bearer auth injected.
struct RouteEntry {
    prefix: &'static str,
    upstream: Upstream,
    target: &'static str,
}

#[derive(Clone, Copy)]
enum Upstream {
    Keymaster,
    NullBoiler,
}

const ROUTES: &[RouteEntry] = &[
    RouteEntry { prefix: "wallet",   upstream: Upstream::Keymaster,  target: "/wallets" },
    RouteEntry { prefix: "balance",  upstream: Upstream::Keymaster,  target: "/balance" },
    RouteEntry { prefix: "balances", upstream: Upstream::Keymaster,  target: "/balances" },
    RouteEntry { prefix: "chain",    upstream: Upstream::Keymaster,  target: "/chain" },
    RouteEntry { prefix: "tx",       upstream: Upstream::Keymaster,  target: "/tx" },
    RouteEntry { prefix: "dispatch", upstream: Upstream::Keymaster,  target: "/dispatch" },
    RouteEntry { prefix: "settings", upstream: Upstream::Keymaster,  target: "/settings" },
    RouteEntry { prefix: "dag",      upstream: Upstream::NullBoiler, target: "/runs" },
];

fn resolve_route<'a>(
    path: &str,
    config: &'a crate::config::GatewayConfig,
) -> Option<(String, String, bool)> {
    // path is already stripped of /api/ prefix by the axum route
    for entry in ROUTES {
        if path == entry.prefix || path.starts_with(&format!("{}/", entry.prefix)) {
            let remainder = &path[entry.prefix.len()..];
            let upstream_url = match entry.upstream {
                Upstream::Keymaster => &config.keymaster_url,
                Upstream::NullBoiler => &config.nullboiler_url,
            };
            let target_path = format!("{}{}", entry.target, remainder);
            let needs_auth = matches!(entry.upstream, Upstream::Keymaster);
            return Some((upstream_url.clone(), target_path, needs_auth));
        }
    }
    None
}

/// Build a nested router for /api/* routes.
/// Uses a fallback on a nested `/api` scope so it doesn't interfere with
/// top-level routes like /trade-up/*, /swarm/*, /health, etc.
pub fn router() -> Router<AppState> {
    Router::new().nest(
        "/api",
        Router::new().fallback(api_proxy_handler),
    )
}

async fn api_proxy_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    let method = req.method().clone();
    let uri_path = req.uri().path().to_string();
    let headers = req.headers().clone();

    // uri_path is already relative to /api (axum strips the nest prefix)
    // so "/api/wallet/xyz" arrives here as "/wallet/xyz"
    let rest = uri_path.trim_start_matches('/');

    // Resolve upstream
    let (upstream_base, target_path, needs_auth) = match resolve_route(rest, &state.config) {
        Some(r) => r,
        None => {
            return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"error": "not found"}))).into_response();
        }
    };

    let url = format!("{}{}", upstream_base.trim_end_matches('/'), target_path);

    // Read body
    let body_bytes = match axum::body::to_bytes(req.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, axum::Json(serde_json::json!({"error": "body too large"}))).into_response();
        }
    };

    // Build upstream request
    let client = reqwest::Client::new();
    let mut upstream_req = match method {
        Method::GET => client.get(&url),
        Method::POST => client.post(&url),
        Method::PUT => client.put(&url),
        Method::DELETE => client.delete(&url),
        Method::PATCH => client.patch(&url),
        _ => client.get(&url),
    };

    // Forward Content-Type
    if let Some(ct) = headers.get("content-type") {
        upstream_req = upstream_req.header("content-type", ct);
    }

    // Inject auth for Keymaster
    if needs_auth && !state.config.keymaster_token.is_empty() {
        upstream_req = upstream_req.header(
            "Authorization",
            format!("Bearer {}", state.config.keymaster_token),
        );
    }

    // Forward body
    if !body_bytes.is_empty() {
        upstream_req = upstream_req.body(body_bytes.to_vec());
    }

    // Execute
    match upstream_req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let ct = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("application/json")
                .to_string();
            match resp.bytes().await {
                Ok(body) => {
                    let is_json = ct.contains("json");
                    let mut response = Response::builder()
                        .status(status)
                        .header("content-type", ct);
                    // If upstream returned non-JSON error, wrap it
                    if !status.is_success() && !is_json {
                        let wrapped = serde_json::json!({
                            "error": String::from_utf8_lossy(&body).to_string()
                        });
                        response = response.header("content-type", "application/json");
                        return response
                            .body(Body::from(serde_json::to_vec(&wrapped).unwrap_or_default()))
                            .unwrap()
                            .into_response();
                    }
                    response.body(Body::from(body)).unwrap().into_response()
                }
                Err(_) => {
                    (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({"error": "upstream read failed"}))).into_response()
                }
            }
        }
        Err(e) => {
            tracing::warn!("proxy upstream error: {e}");
            (StatusCode::BAD_GATEWAY, axum::Json(serde_json::json!({"error": format!("upstream unreachable: {e}")}))).into_response()
        }
    }
}

