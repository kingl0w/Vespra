use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use gateway_rs::agents::executor::ExecutorAgent;
use gateway_rs::agents::risk::RiskAgent;
use gateway_rs::agents::scout::ScoutAgent;
use gateway_rs::agents::sentinel::SentinelAgent;
use gateway_rs::agents::trader::TraderAgent;
use gateway_rs::agents::LlmClient;
use gateway_rs::chain::ChainRegistry;
use gateway_rs::config::GatewayConfig;
use gateway_rs::data::pool::PoolFetcher;
use gateway_rs::data::price::OracleRouter;
use gateway_rs::data::protocol::ProtocolFetcher;
use gateway_rs::data::quote::QuoteFetcher;
use gateway_rs::data::wallet::WalletFetcher;
use gateway_rs::orchestrator::trade_up::TradeUpOrchestrator;
use gateway_rs::routes::{self, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // 2. Load config (scans RPC_URL_* env vars into rpc_urls map)
    let config = GatewayConfig::load().unwrap_or_else(|e| {
        tracing::warn!("config load failed ({e}), using defaults");
        serde_json::from_value(serde_json::json!({})).unwrap()
    });
    let config = Arc::new(config);

    // 3. Init chain registry (receives rpc_urls from config)
    let chain_registry = Arc::new(ChainRegistry::new(&config.rpc_urls));
    let available = chain_registry.available();
    tracing::info!(
        "chain registry: {} chains available: {:?}",
        available.len(),
        available.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    // 4. Init Redis
    let redis_client = Arc::new(redis::Client::open(config.redis_url.as_str())?);
    // Verify connectivity
    match redis::Client::get_multiplexed_async_connection(redis_client.as_ref()).await {
        Ok(_) => tracing::info!("redis connected: {}", config.redis_url),
        Err(e) => {
            tracing::error!("redis connection failed: {e}");
            panic!("redis is required — set VESPRA_REDIS_URL or start redis");
        }
    };

    // 5. Build shared HTTP client
    let http_client = reqwest::Client::builder()
        .user_agent("vespra-gateway-rs/0.1.0")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    // 6. Build data fetchers
    let pool_fetcher = Arc::new(PoolFetcher::new(
        http_client.clone(),
        redis_client.clone(),
        chain_registry.clone(),
    ));
    let protocol_fetcher = Arc::new(ProtocolFetcher::new(
        http_client.clone(),
        redis_client.clone(),
        chain_registry.clone(),
    ));
    let quote_fetcher = Arc::new(QuoteFetcher::new());
    let wallet_fetcher = Arc::new(WalletFetcher::new(
        config.keymaster_url.clone(),
        config.keymaster_token.clone(),
        http_client.clone(),
    ));

    // 7. Build price oracle via config-driven router
    let price_oracle: Arc<dyn gateway_rs::data::price::PriceOracle> =
        Arc::new(OracleRouter::from_config(
            &config,
            chain_registry.clone(),
            redis_client.clone(),
            http_client.clone(),
        ));

    // 8. Build LLM client + agents
    // Separate HTTP client for LLM — disable auto-decompression to avoid
    // "error decoding response body" when DeepSeek returns chunked gzip
    let llm_http = reqwest::Client::builder()
        .user_agent("vespra-gateway-rs/0.1.0")
        .timeout(std::time::Duration::from_secs(120))
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .build()?;
    tracing::info!("LLM provider={} model={} base_url={} api_key len={}",
        config.llm_provider, config.llm_model, config.llm_base_url, config.llm_api_key.len());
    let llm: Arc<dyn gateway_rs::agents::AgentClient> = Arc::new(LlmClient::new(
        llm_http,
        config.llm_base_url.clone(),
        config.llm_api_key.clone(),
        config.llm_model.clone(),
    ));

    let scout = Arc::new(ScoutAgent::new(llm.clone()));
    let risk = Arc::new(RiskAgent::new(llm.clone()));
    let trader = Arc::new(TraderAgent::new(llm.clone()));
    let sentinel = Arc::new(SentinelAgent::new(llm.clone()));
    let executor = Arc::new(ExecutorAgent::new(
        config.keymaster_url.clone(),
        config.keymaster_token.clone(),
        http_client.clone(),
    ));

    // 9. Build shared kill flag + orchestrator
    let kill_flag = Arc::new(AtomicBool::new(false));
    let trade_up_orchestrator = Arc::new(TradeUpOrchestrator::new(
        pool_fetcher,
        protocol_fetcher,
        price_oracle,
        wallet_fetcher,
        quote_fetcher,
        scout,
        risk,
        trader,
        sentinel,
        executor,
        config.clone(),
        chain_registry.clone(),
        redis_client.clone(),
        kill_flag.clone(),
    ));

    // 10. Build app state and router
    let state = AppState {
        config: config.clone(),
        chain_registry,
        redis: redis_client,
        trade_up_orchestrator,
        kill_flag,
    };

    let app = routes::router(state);

    // 11. Serve
    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("gateway-rs listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
