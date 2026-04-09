use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::executor::ExecutorAgent;
use crate::config::GatewayConfig;
use crate::orchestrator::trade_up::TradeUpOrchestrator;
use crate::orchestrator::yield_rot::YieldOrchestrator;

//─── types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CustodyMode {
    Safe,
    Operator,
}

impl Default for CustodyMode {
    fn default() -> Self {
        CustodyMode::Safe
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub strategy: String,
    pub pct: f64,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioSpec {
    pub source_wallet_id: String,
    pub total_eth: f64,
    pub chain: String,
    pub custody: CustodyMode,
    pub gas_reserve_per_wallet: f64,
    pub allocations: Vec<Allocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletAllocation {
    pub wallet_id: String,
    pub address: String,
    pub label: String,
    pub strategy: String,
    pub allocated_eth: f64,
    pub gas_reserve_eth: f64,
    pub funded_tx_hash: Option<String>,
    pub loop_started: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioReport {
    pub portfolio_id: String,
    pub source_wallet_id: String,
    pub total_eth: f64,
    pub chain: String,
    pub custody: CustodyMode,
    pub wallets: Vec<WalletAllocation>,
    pub errors: Vec<String>,
    pub created_at: i64,
}

//─── orchestrator ────────────────────────────────────────────────

pub struct PortfolioOrchestrator {
    executor: Arc<ExecutorAgent>,
    trade_up: Arc<TradeUpOrchestrator>,
    yield_orch: Arc<YieldOrchestrator>,
    config: Arc<GatewayConfig>,
    redis: Arc<redis::Client>,
    kill_flag: Arc<AtomicBool>,
}

impl PortfolioOrchestrator {
    pub fn new(
        executor: Arc<ExecutorAgent>,
        trade_up: Arc<TradeUpOrchestrator>,
        yield_orch: Arc<YieldOrchestrator>,
        config: Arc<GatewayConfig>,
        redis: Arc<redis::Client>,
        kill_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            executor,
            trade_up,
            yield_orch,
            config,
            redis,
            kill_flag,
        }
    }

    pub async fn deploy(&self, spec: PortfolioSpec) -> PortfolioReport {
        let portfolio_id = Uuid::new_v4().to_string();
        let mut wallets = Vec::new();
        let mut errors = Vec::new();

        //gate: kill flag
        if self.kill_flag.load(Ordering::SeqCst) {
            return PortfolioReport {
                portfolio_id,
                source_wallet_id: spec.source_wallet_id,
                total_eth: spec.total_eth,
                chain: spec.chain,
                custody: spec.custody,
                wallets: vec![],
                errors: vec!["kill_switch_active".into()],
                created_at: chrono::Utc::now().timestamp(),
            };
        }

        //validate allocations sum to 100
        let total_pct: f64 = spec.allocations.iter().map(|a| a.pct).sum();
        if (total_pct - 100.0).abs() > 0.01 {
            return PortfolioReport {
                portfolio_id,
                source_wallet_id: spec.source_wallet_id,
                total_eth: spec.total_eth,
                chain: spec.chain,
                custody: spec.custody,
                wallets: vec![],
                errors: vec![format!("allocations sum to {total_pct}%, must be 100%")],
                created_at: chrono::Utc::now().timestamp(),
            };
        }

        if spec.total_eth <= 0.0 {
            return PortfolioReport {
                portfolio_id,
                source_wallet_id: spec.source_wallet_id,
                total_eth: spec.total_eth,
                chain: spec.chain,
                custody: spec.custody,
                wallets: vec![],
                errors: vec!["total_eth must be > 0".into()],
                created_at: chrono::Utc::now().timestamp(),
            };
        }

        let source_uuid = match Uuid::parse_str(&spec.source_wallet_id) {
            Ok(id) => id,
            Err(e) => {
                return PortfolioReport {
                    portfolio_id,
                    source_wallet_id: spec.source_wallet_id,
                    total_eth: spec.total_eth,
                    chain: spec.chain,
                    custody: spec.custody,
                    wallets: vec![],
                    errors: vec![format!("invalid source_wallet_id: {e}")],
                    created_at: chrono::Utc::now().timestamp(),
                };
            }
        };

        let custody_str = match spec.custody {
            CustodyMode::Safe => "safe",
            CustodyMode::Operator => "operator",
        };

        //process each allocation
        for alloc in &spec.allocations {
            //kill flag check per-wallet
            if self.kill_flag.load(Ordering::SeqCst) {
                errors.push("kill_switch_activated_mid_deploy".into());
                break;
            }

            let allocated_eth = (alloc.pct / 100.0) * spec.total_eth;
            if allocated_eth <= spec.gas_reserve_per_wallet {
                errors.push(format!(
                    "{}: allocated {:.4} ETH <= gas reserve {:.4} ETH",
                    alloc.label, allocated_eth, spec.gas_reserve_per_wallet
                ));
                continue;
            }

            //1. create wallet via keymaster
            let create_payload = serde_json::json!({
                "label": alloc.label,
                "chain": spec.chain,
                "custody_mode": custody_str,
            });
            let wallet_resp = match self.create_wallet(&create_payload).await {
                Ok(resp) => resp,
                Err(e) => {
                    errors.push(format!("{}: wallet creation failed: {e}", alloc.label));
                    continue;
                }
            };

            let wallet_id = wallet_resp
                .get("wallet_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let address = wallet_resp
                .get("address")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if wallet_id.is_empty() {
                errors.push(format!("{}: no wallet_id in response", alloc.label));
                continue;
            }

            //2. fund wallet — send (allocated - gas_reserve) from source
            let fund_eth = allocated_eth - spec.gas_reserve_per_wallet;
            let fund_wei = format!("{:.0}", fund_eth * 1e18);
            let fund_result = self
                .executor
                .execute(source_uuid, "ETH", &address, &fund_wei, &spec.chain)
                .await;

            let funded_tx_hash = match fund_result {
                Ok(r) => r.tx_hash,
                Err(e) => {
                    errors.push(format!("{}: fund transfer failed: {e}", alloc.label));
                    None
                }
            };

            //3. spawn strategy
            let new_wallet_uuid = Uuid::parse_str(&wallet_id).unwrap_or_default();
            let loop_started = match alloc.strategy.as_str() {
                "trade_up" => {
                    match self
                        .trade_up
                        .start_loop(
                            new_wallet_uuid,
                            fund_eth,
                            vec![spec.chain.clone()],
                        )
                        .await
                    {
                        Ok(()) => true,
                        Err(e) => {
                            errors.push(format!("{}: trade_up start failed: {e}", alloc.label));
                            false
                        }
                    }
                }
                "yield" => {
                    match self
                        .yield_orch
                        .start_loop(new_wallet_uuid, fund_eth, spec.chain.clone())
                        .await
                    {
                        Ok(()) => true,
                        Err(e) => {
                            errors.push(format!("{}: yield start failed: {e}", alloc.label));
                            false
                        }
                    }
                }
                "sniper" => {
                    //sniper is webhook-driven, just note it
                    tracing::info!("portfolio: sniper wallet {} provisioned (webhook-driven)", alloc.label);
                    true
                }
                other => {
                    errors.push(format!("{}: unknown strategy '{other}'", alloc.label));
                    false
                }
            };

            wallets.push(WalletAllocation {
                wallet_id,
                address,
                label: alloc.label.clone(),
                strategy: alloc.strategy.clone(),
                allocated_eth,
                gas_reserve_eth: spec.gas_reserve_per_wallet,
                funded_tx_hash,
                loop_started,
            });
        }

        let report = PortfolioReport {
            portfolio_id: portfolio_id.clone(),
            source_wallet_id: spec.source_wallet_id,
            total_eth: spec.total_eth,
            chain: spec.chain,
            custody: spec.custody,
            wallets,
            errors,
            created_at: chrono::Utc::now().timestamp(),
        };

        //persist to redis
        self.persist_portfolio(&portfolio_id, &report).await;

        report
    }

    pub async fn exit_portfolio(
        &self,
        portfolio_id: &str,
    ) -> serde_json::Value {
        let report = match self.load_portfolio(portfolio_id).await {
            Some(r) => r,
            None => {
                return serde_json::json!({
                    "status": "error",
                    "error": "portfolio_not_found",
                });
            }
        };

        let _source_uuid = Uuid::parse_str(&report.source_wallet_id).unwrap_or_default();
        let mut stopped = Vec::new();
        let mut sweep_txs = Vec::new();
        let mut exit_errors = Vec::new();

        for wallet in &report.wallets {
            let wid = Uuid::parse_str(&wallet.wallet_id).unwrap_or_default();

            //stop strategy loop
            match wallet.strategy.as_str() {
                "trade_up" => { let _ = self.trade_up.stop_loop(wid).await; }
                "yield" => { let _ = self.yield_orch.stop_loop(wid).await; }
                _ => {}
            }
            stopped.push(wallet.label.clone());

            //sweep funds back to source
            let sweep_wei = format!("{:.0}", wallet.allocated_eth * 1e18);
            match self
                .executor
                .execute(wid, "ETH", &report.source_wallet_id, &sweep_wei, &report.chain)
                .await
            {
                Ok(r) => {
                    if let Some(tx) = r.tx_hash {
                        sweep_txs.push(serde_json::json!({
                            "wallet": wallet.label,
                            "tx_hash": tx,
                        }));
                    }
                }
                Err(e) => {
                    exit_errors.push(format!("{}: sweep failed: {e}", wallet.label));
                }
            }
        }

        serde_json::json!({
            "status": "exited",
            "portfolio_id": portfolio_id,
            "stopped_loops": stopped,
            "sweep_txs": sweep_txs,
            "errors": exit_errors,
        })
    }

    //── keymaster wallet creation ────────────────────────────────

    async fn create_wallet(
        &self,
        payload: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let resp = reqwest::Client::new()
            .post(format!("{}/wallets", self.config.keymaster_url))
            .header(
                "Authorization",
                format!("Bearer {}", self.config.keymaster_token),
            )
            .json(payload)
            .send()
            .await?;

        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }

    //── redis persistence ────────────────────────────────────────

    async fn persist_portfolio(&self, portfolio_id: &str, report: &PortfolioReport) {
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            if let Ok(json) = serde_json::to_string(report) {
                let key = format!("vespra:portfolios:{portfolio_id}");
                let _: Result<(), _> = conn.set::<_, _, ()>(&key, &json).await;
            }
        }
    }

    pub async fn load_portfolio(&self, portfolio_id: &str) -> Option<PortfolioReport> {
        let mut conn = redis::Client::get_multiplexed_async_connection(self.redis.as_ref())
            .await
            .ok()?;
        let key = format!("vespra:portfolios:{portfolio_id}");
        let raw: Option<String> = conn.get(&key).await.ok().flatten();
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }

    pub async fn list_portfolios(&self) -> Vec<PortfolioReport> {
        if let Ok(mut conn) =
            redis::Client::get_multiplexed_async_connection(self.redis.as_ref()).await
        {
            let keys: Vec<String> = redis::cmd("KEYS")
                .arg("vespra:portfolios:*")
                .query_async(&mut conn)
                .await
                .unwrap_or_default();

            let mut portfolios = Vec::new();
            for key in keys {
                let raw: Option<String> = conn.get(&key).await.ok().flatten();
                if let Some(report) = raw.and_then(|s| serde_json::from_str(&s).ok()) {
                    portfolios.push(report);
                }
            }
            portfolios
        } else {
            vec![]
        }
    }

    pub async fn portfolio_with_status(
        &self,
        portfolio_id: &str,
        trade_up: &TradeUpOrchestrator,
        yield_orch: &YieldOrchestrator,
    ) -> Option<serde_json::Value> {
        let report = self.load_portfolio(portfolio_id).await?;
        let trade_up_active = trade_up.active_wallets().await;
        let yield_active = yield_orch.active_wallets().await;

        let wallets_with_status: Vec<serde_json::Value> = report
            .wallets
            .iter()
            .map(|w| {
                let wid = Uuid::parse_str(&w.wallet_id).unwrap_or_default();
                let loop_active = match w.strategy.as_str() {
                    "trade_up" => trade_up_active.contains(&wid),
                    "yield" => yield_active.contains(&wid),
                    "sniper" => true, // webhook-driven
                    _ => false,
                };
                serde_json::json!({
                    "wallet_id": w.wallet_id,
                    "address": w.address,
                    "label": w.label,
                    "strategy": w.strategy,
                    "allocated_eth": w.allocated_eth,
                    "gas_reserve_eth": w.gas_reserve_eth,
                    "funded_tx_hash": w.funded_tx_hash,
                    "loop_active": loop_active,
                })
            })
            .collect();

        Some(serde_json::json!({
            "portfolio_id": report.portfolio_id,
            "source_wallet_id": report.source_wallet_id,
            "total_eth": report.total_eth,
            "chain": report.chain,
            "custody": report.custody,
            "wallets": wallets_with_status,
            "errors": report.errors,
            "created_at": report.created_at,
        }))
    }
}
