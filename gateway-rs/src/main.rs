use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;

use gateway_rs::agents::coordinator::CoordinatorAgent;
use gateway_rs::agents::executor::ExecutorAgent;
use gateway_rs::agents::launcher::LauncherAgent;
use gateway_rs::agents::risk::RiskAgent;
use gateway_rs::agents::scout::ScoutAgent;
use gateway_rs::agents::sentinel::SentinelAgent;
use gateway_rs::agents::sniper::SniperAgent;
use gateway_rs::agents::trader::TraderAgent;
use gateway_rs::agents::yield_agent::YieldAgent;
use gateway_rs::agents::LlmClient;
use gateway_rs::chain::ChainRegistry;
use gateway_rs::config::GatewayConfig;
use gateway_rs::data::aave::AaveFetcher;
use gateway_rs::data::historical::{CompositeHistoricalFeed, HistoricalFeed};
use gateway_rs::data::pool::PoolFetcher;
use gateway_rs::data::price::OracleRouter;
use gateway_rs::data::protocol::ProtocolFetcher;
use gateway_rs::data::quote::QuoteFetcher;
use gateway_rs::data::wallet::WalletFetcher;
use gateway_rs::data::yield_provider::ProviderRegistry;
use gateway_rs::orchestrator::command::CommandOrchestrator;
use gateway_rs::orchestrator::coordinator::CoordinatorOrchestrator;
use gateway_rs::orchestrator::launcher::LauncherOrchestrator;
use gateway_rs::orchestrator::portfolio::PortfolioOrchestrator;
use gateway_rs::orchestrator::sniper::SniperOrchestrator;
use gateway_rs::orchestrator::trade_up::TradeUpOrchestrator;
use gateway_rs::orchestrator::yield_rot::YieldOrchestrator;
use gateway_rs::routes::{self, AppState};
use gateway_rs::sentinel_monitor::SentinelMonitor;
use gateway_rs::yield_scheduler::YieldScheduler;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    //1. init tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = match GatewayConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(
                "config load failed ({e}) — running on defaults, verify your config file"
            );
            serde_json::from_value(serde_json::json!({})).unwrap()
        }
    };
    let config = Arc::new(config);

    //3. init chain registry (receives rpc_urls from config)
    let chain_registry = Arc::new(ChainRegistry::new(&config.rpc_urls));
    let available = chain_registry.available();
    tracing::info!(
        "chain registry: {} chains available: {:?}",
        available.len(),
        available.iter().map(|c| &c.name).collect::<Vec<_>>()
    );

    //4. init redis
    let redis_client = Arc::new(redis::Client::open(config.redis_url.as_str())?);
    //verify connectivity
    match redis::Client::get_multiplexed_async_connection(redis_client.as_ref()).await {
        Ok(_) => tracing::info!("redis connected: {}", config.redis_url),
        Err(e) => {
            tracing::error!("redis connection failed: {e}");
            panic!("redis is required — set VESPRA_REDIS_URL or start redis");
        }
    };

    //5. build shared http client
    let http_client = reqwest::Client::builder()
        .user_agent("vespra-gateway-rs/0.1.0")
        .timeout(std::time::Duration::from_secs(30))
        .pool_max_idle_per_host(5)
        .build()?;

    //6. build data fetchers
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
    let quote_fetcher = Arc::new(QuoteFetcher::from_config(
        http_client.clone(),
        &config,
        chain_registry.clone(),
    ));
    let wallet_fetcher = Arc::new(WalletFetcher::new(
        config.keymaster_url.clone(),
        config.keymaster_token.clone(),
        http_client.clone(),
        chain_registry.clone(),
    ));

    //6b. build yield provider registry
    let yield_registry = Arc::new(ProviderRegistry::from_config(
        &config,
        http_client.clone(),
        redis_client.clone(),
    ));

    //6c. build aave v3 fetcher
    let aave_fetcher = Arc::new(AaveFetcher::new(
        http_client.clone(),
        redis_client.clone(),
    ));

    //6d. build historical data feed (defillama apy + coingecko price) for the
    //backtesting engine.
    let historical_feed: Arc<dyn HistoricalFeed> =
        Arc::new(CompositeHistoricalFeed::new(http_client.clone()));

    //7. build price oracle via config-driven router
    let price_oracle: Arc<dyn gateway_rs::data::price::PriceOracle> =
        Arc::new(OracleRouter::from_config(
            &config,
            chain_registry.clone(),
            redis_client.clone(),
            http_client.clone(),
        ));

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

    let scout = Arc::new(
        ScoutAgent::new(llm.clone())
            .with_yield_registry(yield_registry.clone(), config.clone()),
    );
    let risk = Arc::new(RiskAgent::new(llm.clone()));
    let trader = Arc::new(TraderAgent::new(llm.clone()));
    let sentinel = Arc::new(SentinelAgent::new(
        llm.clone(),
        config.keymaster_url.clone(),
        config.keymaster_token.clone(),
        http_client.clone(),
    ));
    let yield_agent = Arc::new(
        YieldAgent::new(llm.clone())
            .with_live_data(aave_fetcher.clone(), yield_registry.clone(), config.clone()),
    );
    let sniper_agent = Arc::new(SniperAgent::new(llm.clone()));
    let coordinator_agent = Arc::new(CoordinatorAgent::new(llm.clone()));
    let launcher_agent = Arc::new(LauncherAgent::new(llm.clone()));
    let executor = Arc::new(ExecutorAgent::new(
        config.keymaster_url.clone(),
        config.keymaster_token.clone(),
        http_client.clone(),
        config.clone(),
    ));

    //8b. build goalrunner dependencies (before orchestrators consume arcs)
    let dry_run = std::env::var("VESPRA_DRY_RUN")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if dry_run {
        tracing::info!("VESPRA_DRY_RUN=true — GoalRunner will skip Keymaster calls");
    }
    let goal_runner_deps = gateway_rs::goal_runner::GoalRunnerDeps {
        pool_fetcher: pool_fetcher.clone(),
        protocol_fetcher: protocol_fetcher.clone(),
        price_oracle: price_oracle.clone(),
        wallet_fetcher: wallet_fetcher.clone(),
        quote_fetcher: quote_fetcher.clone(),
        scout: scout.clone(),
        risk: risk.clone(),
        trader: trader.clone(),
        sentinel: sentinel.clone(),
        executor: executor.clone(),
        config: config.clone(),
        chain_registry: chain_registry.clone(),
        redis: redis_client.clone(),
        dry_run,
    };

    //9. build shared kill flag + orchestrator
    let kill_flag = Arc::new(AtomicBool::new(false));
    let trade_up_orchestrator = Arc::new(TradeUpOrchestrator::new(
        pool_fetcher.clone(),
        protocol_fetcher.clone(),
        price_oracle,
        wallet_fetcher,
        quote_fetcher.clone(),
        scout.clone(),
        risk.clone(),
        trader.clone(),
        sentinel.clone(),
        executor.clone(),
        config.clone(),
        chain_registry.clone(),
        redis_client.clone(),
        kill_flag.clone(),
    ));

    //9b. build yield orchestrator
    let yield_orchestrator = Arc::new(YieldOrchestrator::new(
        pool_fetcher,
        protocol_fetcher.clone(),
        risk.clone(),
        yield_agent.clone(),
        executor.clone(),
        config.clone(),
        redis_client.clone(),
        kill_flag.clone(),
    ));

    //9c. build sniper orchestrator
    let sniper_orchestrator = Arc::new(SniperOrchestrator::new(
        risk.clone(),
        sniper_agent.clone(),
        executor.clone(),
        protocol_fetcher,
        quote_fetcher,
        chain_registry.clone(),
        config.clone(),
        redis_client.clone(),
        kill_flag.clone(),
    ));

    //9d. build command orchestrator
    let command_orchestrator = Arc::new(CommandOrchestrator::new(
        coordinator_agent,
        trade_up_orchestrator.clone(),
        yield_orchestrator.clone(),
        config.clone(),
        kill_flag.clone(),
        scout.clone(),
        risk.clone(),
        sentinel.clone(),
        trader.clone(),
        yield_agent.clone(),
        sniper_agent.clone(),
        launcher_agent.clone(),
    ));

    //9e. build coordinator orchestrator
    let coordinator_orchestrator = Arc::new(CoordinatorOrchestrator::new(
        llm.clone(),
        redis_client.clone(),
        config.clone(),
        yield_registry.clone(),
    ));

    //9f. build launcher orchestrator
    let launcher_orchestrator = Arc::new(LauncherOrchestrator::new(
        launcher_agent,
        executor.clone(),
        config.clone(),
        redis_client.clone(),
        kill_flag.clone(),
    ));

    //9f. build portfolio orchestrator
    let portfolio_orchestrator = Arc::new(PortfolioOrchestrator::new(
        executor,
        trade_up_orchestrator.clone(),
        yield_orchestrator.clone(),
        config.clone(),
        redis_client.clone(),
        kill_flag.clone(),
    ));

    //10. build rate limiters + app state
    let webhook_rate_limiter = Arc::new(
        gateway_rs::routes::ratelimit::WebhookRateLimiter::new(config.rl_webhook_rpm),
    );
    let route_limiters =
        gateway_rs::middleware::rate_limit::RouteLimiters::from_config(&config);
    tracing::info!(
        "rate limits: enabled={} agent={}rpm wallet_create={}rph tx_send={}rph | webhook={}rpm cors={} cf_access={}",
        config.rate_limit_enabled,
        config.rate_limit_agent_rpm,
        config.rate_limit_wallet_create_rph,
        config.rate_limit_tx_send_rph,
        config.rl_webhook_rpm, config.cors_origin, config.cf_access_required,
    );

    let state = AppState {
        config: config.clone(),
        chain_registry,
        redis: redis_client,
        http_client: http_client.clone(),
        llm: llm.clone(),
        trade_up_orchestrator,
        yield_orchestrator,
        sniper_orchestrator,
        command_orchestrator,
        launcher_orchestrator,
        portfolio_orchestrator,
        kill_flag,
        webhook_rate_limiter,
        yield_registry,
        aave_fetcher,
        yield_agent: yield_agent.clone(),
        route_limiters,
        coordinator_orchestrator,
        goal_runners: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        goal_cancel_txs: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        goal_creation_lock: Arc::new(tokio::sync::Mutex::new(())),
        goal_runner_deps,
        sentinel_monitor: Arc::new(SentinelMonitor::new()),
        yield_scheduler_status: gateway_rs::yield_scheduler::default_status(),
        historical_feed,
    };

    //10a. auto-resume running/paused goals from previous boot
    let auto_resume = std::env::var("VESPRA_AUTO_RESUME_GOALS")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    if auto_resume {
        match gateway_rs::routes::goals::list_goals_all(&state.redis).await {
            Ok(goals) => {
                let resumable: Vec<_> = goals
                    .into_iter()
                    .filter(|g| {
                        matches!(g.status,
                            gateway_rs::types::goals::GoalStatus::Running
                            | gateway_rs::types::goals::GoalStatus::Paused
                        )
                    })
                    .collect();

                let mut from_monitoring = 0u32;
                let mut from_scouting = 0u32;
                let mut paused_count = 0u32;

                for goal in &resumable {
                    let truncated: String = goal.raw_goal.chars().take(60).collect();
                    tracing::info!("[boot] resuming goal {}: {truncated}", goal.id);

                    if goal.status == gateway_rs::types::goals::GoalStatus::Paused {
                        paused_count += 1;
                    }

                    let resume_step = match goal.current_step.as_str() {
                        "MONITORING" | "EXITING" => {
                            from_monitoring += 1;
                            Some(gateway_rs::goal_runner::GoalStep::Monitoring)
                        }
                        "EXECUTING" => {
                            from_monitoring += 1;
                            tracing::warn!(
                                "[boot] goal {} crashed mid-execution, resuming from MONITORING \
                                 - verify position manually",
                                goal.id
                            );
                            Some(gateway_rs::goal_runner::GoalStep::Monitoring)
                        }
                        _ => {
                            //scouting, risk, trading, or unknown → start from beginning
                            from_scouting += 1;
                            None
                        }
                    };

                    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
                    let goal_id = goal.id;
                    let deps = state.goal_runner_deps.clone();
                    let runners_for_cleanup = state.goal_runners.clone();
                    let txs_for_cleanup = state.goal_cancel_txs.clone();
                    let handle = tokio::spawn(async move {
                        gateway_rs::goal_runner::run_goal_with_resume(
                            goal_id, cancel_rx, deps, resume_step,
                        )
                        .await;
                        //ves-mem: clean up shared maps when the runner exits.
                        runners_for_cleanup.lock().await.remove(&goal_id);
                        txs_for_cleanup.lock().await.remove(&goal_id);
                    });

                    {
                        let mut runners = state.goal_runners.lock().await;
                        runners.insert(goal_id, handle);
                    }
                    {
                        let mut txs = state.goal_cancel_txs.lock().await;
                        txs.insert(goal_id, cancel_tx);
                    }
                }

                let total = resumable.len() as u32;
                tracing::info!(
                    "[boot] resumed {total} goals ({from_monitoring} from monitoring, \
                     {from_scouting} from scouting, {paused_count} paused)"
                );

                //store boot resume report in redis
                let report = serde_json::json!({
                    "booted_at": chrono::Utc::now().to_rfc3339(),
                    "goals_resumed": total,
                    "from_monitoring": from_monitoring,
                    "from_scouting": from_scouting,
                    "paused_count": paused_count,
                });
                if let Ok(mut conn) =
                    redis::Client::get_multiplexed_async_connection(state.redis.as_ref()).await
                {
                    let _: Result<(), _> = redis::AsyncCommands::set(
                        &mut conn,
                        "boot:last_resume_report",
                        report.to_string(),
                    )
                    .await;
                }
            }
            Err(e) => {
                tracing::error!("[boot] failed to load goals for resume: {e}");
            }
        }
    } else {
        tracing::info!("[boot] VESPRA_AUTO_RESUME_GOALS=false — skipping goal resume");
    }

    //10b. spawn sentinelmonitor background task
    tokio::spawn(SentinelMonitor::run(
        state.sentinel_monitor.clone(),
        state.redis.clone(),
        sentinel.clone(),
        state.goal_runner_deps.price_oracle.clone(),
    ));

    //10c. spawn yieldrotationscheduler background task
    let yield_sched = Arc::new(YieldScheduler::new(state.yield_scheduler_status.clone()));
    tokio::spawn(YieldScheduler::run(
        yield_sched,
        state.redis.clone(),
        state.aave_fetcher.clone(),
        state.yield_registry.clone(),
    ));

    let app = routes::router(state);

    //11. serve
    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("gateway-rs listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
