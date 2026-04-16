mod auth;
mod config;
mod crypto;
mod error;
mod keystore;
mod routes;
mod rpc;
mod state;
mod swap;

use axum::{
    middleware,
    routing::{delete, get, post, put},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::keystore::Keystore;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = std::env::var("VESPRA_KM_CONFIG")
        .map(std::path::PathBuf::from)
        .ok();
    let config = Config::load(config_path.as_deref());

    let master_password = std::env::var("VESPRA_MASTER_PASSWORD").unwrap_or_else(|_| {
        eprintln!("ERROR: VESPRA_MASTER_PASSWORD environment variable is required");
        eprintln!("Generate one with: openssl rand -base64 32");
        std::process::exit(1);
    });

    if master_password.len() < 16 {
        eprintln!("ERROR: VESPRA_MASTER_PASSWORD must be at least 16 characters");
        std::process::exit(1);
    }

    let auth_token = std::env::var("VESPRA_KM_AUTH_TOKEN").unwrap_or_else(|_| {
        eprintln!("ERROR: VESPRA_KM_AUTH_TOKEN environment variable is required");
        eprintln!("Generate one with: openssl rand -base64 32");
        std::process::exit(1);
    });

    if auth_token.len() < 16 {
        eprintln!("ERROR: VESPRA_KM_AUTH_TOKEN must be at least 16 characters");
        std::process::exit(1);
    }

    let keystore = Keystore::open(&config.db_path).unwrap_or_else(|e| {
        eprintln!("ERROR: Failed to open keystore database: {e}");
        std::process::exit(1);
    });

    //validate fee config before proceeding
    if let Err(msg) = config::validate_fee_config(&config) {
        tracing::error!("{msg}");
        std::process::exit(1);
    }

    if config.fees_enabled {
        let treasury = config.treasury_address.as_deref().unwrap_or("");
        tracing::info!(
            "[fees] enabled — per-tx=500bps, aum=50bps annual, treasury={treasury}"
        );
    } else {
        tracing::info!("[fees] disabled — running in free/self-hosted mode");
    }

    let active_chains = config.active_chains();
    tracing::info!(
        chains = ?active_chains.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        db = %config.db_path.display(),
        "Vespra Keymaster starting"
    );

    let state = Arc::new(AppState {
        config: config.clone(), keystore, master_password, auth_token,
    });

    //public read-only routes — no auth required
    let public_routes = Router::new()
        .route("/health", get(routes::health))
        .route("/wallets", get(routes::list_wallets))
        .route("/wallets/:wallet_id", get(routes::get_wallet))
        .route("/balance/:chain/:address", get(routes::get_balance))
        .route("/balances/:chain", get(routes::get_all_balances))
        .route("/chain/:chain", get(routes::chain_status))
        .route("/tx/log/:wallet_id", get(routes::get_tx_log))
        .route("/fees/aum", get(routes::fees_aum))
        .route("/fees/summary", get(routes::fees_summary));

    //protected write routes — bearer token required
    let protected_routes = Router::new()
        .route("/wallets", post(routes::create_wallet))
        .route("/wallets/:wallet_id", delete(routes::deactivate_wallet))
        .route("/wallets/:wallet_id/cap", put(routes::update_cap))
        .route("/wallets/:wallet_id/cap/reset", post(routes::reset_cap))
        .route("/tx/send", post(routes::send_native))
        .route("/tx/send_tx", post(routes::send_tx_with_data))
        .route("/tx/sweep", post(routes::sweep_to_safe))
        .route("/swap", post(routes::swap_handler))
        .route("/dispatch", post(routes::dispatch))
        .route("/settings/safes", get(routes::get_safes))
        .route("/settings/safes/:chain", put(routes::set_safe))
        .layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    //clone state for aum fee background task before moving into router
    let aum_state = Arc::clone(&state);

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes)
        .layer(axum::extract::DefaultBodyLimit::max(65536)) // 64KB
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    //ves-111: surface bind-address parse failures via the normal error path
    //so callers can react instead of the process vanishing with exit(1).
    let addr: SocketAddr = format!("{}:{}", config.host, config.port)
        .parse()
        .map_err(|e| {
            anyhow::anyhow!(
                "invalid bind address {}:{} — {e}",
                config.host,
                config.port
            )
        })?;

    //aum fee sweep background task — ves-56
    if config.fees_enabled {
        tokio::spawn(async move {
            routes::aum_sweep_loop(aum_state).await;
        });
        tracing::info!("[aum_fee] sweep thread spawned (interval=7d)");
    } else {
        tracing::info!("[aum_fee] sweep thread disabled — fees off");
    }

    tracing::info!(%addr, "Vespra Keymaster listening");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap_or_else(|e| {
        eprintln!("ERROR: Failed to bind {addr}: {e}");
        std::process::exit(1);
    });
    axum::serve(listener, app).await.unwrap_or_else(|e| {
        eprintln!("ERROR: Server failed: {e}");
        std::process::exit(1);
    });

    Ok(())
}
