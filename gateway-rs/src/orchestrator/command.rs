use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agents::coordinator::{CoordinatorAgent, CoordinatorContext, SystemState};
use crate::config::GatewayConfig;
use crate::orchestrator::trade_up::TradeUpOrchestrator;
use crate::orchestrator::yield_rot::YieldOrchestrator;

// ─── Types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandReport {
    pub strategy: String,
    pub action_taken: String,
    pub params_used: serde_json::Value,
    pub reasoning: String,
}

// ─── Orchestrator ────────────────────────────────────────────────

pub struct CommandOrchestrator {
    coordinator: Arc<CoordinatorAgent>,
    trade_up: Arc<TradeUpOrchestrator>,
    yield_orch: Arc<YieldOrchestrator>,
    config: Arc<GatewayConfig>,
    kill_flag: Arc<AtomicBool>,
}

impl CommandOrchestrator {
    pub fn new(
        coordinator: Arc<CoordinatorAgent>,
        trade_up: Arc<TradeUpOrchestrator>,
        yield_orch: Arc<YieldOrchestrator>,
        config: Arc<GatewayConfig>,
        kill_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            coordinator,
            trade_up,
            yield_orch,
            config,
            kill_flag,
        }
    }

    pub async fn execute(
        &self,
        command: String,
        wallet_id: Option<String>,
    ) -> CommandReport {
        // Gather system state
        let trade_up_wallets = self.trade_up.active_wallets().await;
        let yield_wallets = self.yield_orch.active_wallets().await;
        let mut active_loops: Vec<String> = trade_up_wallets
            .iter()
            .map(|id| format!("trade_up:{id}"))
            .collect();
        active_loops.extend(yield_wallets.iter().map(|id| format!("yield:{id}")));

        let system_state = SystemState {
            active_loops: active_loops.clone(),
            kill_flag: self.kill_flag.load(Ordering::SeqCst),
            wallet_count: trade_up_wallets.len() + yield_wallets.len(),
            chains: self.config.chains.clone(),
        };

        // Run coordinator agent
        let ctx = CoordinatorContext {
            command: command.clone(),
            system_state,
        };
        let intent = match self.coordinator.interpret(&ctx).await {
            Ok(i) => i,
            Err(e) => {
                return CommandReport {
                    strategy: "Error".into(),
                    action_taken: "coordinator_failed".into(),
                    params_used: serde_json::json!({"error": e.to_string()}),
                    reasoning: format!("LLM coordinator failed: {e}"),
                };
            }
        };

        // Determine wallet_id — prefer explicit param, then LLM extraction
        let resolved_wallet_id = wallet_id
            .as_deref()
            .or(intent.wallet_id.as_deref())
            .and_then(|s| Uuid::parse_str(s).ok());

        let params = serde_json::json!({
            "strategy": intent.strategy,
            "wallet_id": resolved_wallet_id.map(|id| id.to_string()),
            "capital_eth": intent.capital_eth,
            "chain": intent.chain,
            "max_eth": intent.max_eth,
            "stop_loss_pct": intent.stop_loss_pct,
            "threshold_pct": intent.threshold_pct,
        });

        // Route by strategy
        match intent.strategy.as_str() {
            "TradeUp" => {
                let Some(wid) = resolved_wallet_id else {
                    return CommandReport {
                        strategy: "TradeUp".into(),
                        action_taken: "failed".into(),
                        params_used: params,
                        reasoning: "wallet_id required for TradeUp".into(),
                    };
                };
                let capital = intent.capital_eth.unwrap_or(0.01);
                let chains = intent.chain
                    .as_ref()
                    .map(|c| vec![c.clone()])
                    .unwrap_or_else(|| self.config.chains.clone());

                match self.trade_up.start_loop(wid, capital, chains).await {
                    Ok(()) => CommandReport {
                        strategy: "TradeUp".into(),
                        action_taken: "loop_started".into(),
                        params_used: params,
                        reasoning: intent.reasoning,
                    },
                    Err(e) => CommandReport {
                        strategy: "TradeUp".into(),
                        action_taken: format!("start_failed: {e}"),
                        params_used: params,
                        reasoning: intent.reasoning,
                    },
                }
            }
            "YieldRotate" => {
                let Some(wid) = resolved_wallet_id else {
                    return CommandReport {
                        strategy: "YieldRotate".into(),
                        action_taken: "failed".into(),
                        params_used: params,
                        reasoning: "wallet_id required for YieldRotate".into(),
                    };
                };
                let capital = intent.capital_eth.unwrap_or(0.01);
                let chain = intent.chain.unwrap_or_else(|| "base".into());

                match self.yield_orch.start_loop(wid, capital, chain).await {
                    Ok(()) => CommandReport {
                        strategy: "YieldRotate".into(),
                        action_taken: "loop_started".into(),
                        params_used: params,
                        reasoning: intent.reasoning,
                    },
                    Err(e) => CommandReport {
                        strategy: "YieldRotate".into(),
                        action_taken: format!("start_failed: {e}"),
                        params_used: params,
                        reasoning: intent.reasoning,
                    },
                }
            }
            "Sniper" => {
                CommandReport {
                    strategy: "Sniper".into(),
                    action_taken: "sniper_noted".into(),
                    params_used: params,
                    reasoning: format!("{} — sniper activation is webhook-driven, not loop-based", intent.reasoning),
                }
            }
            "Kill" => {
                self.kill_flag.store(true, Ordering::SeqCst);
                tracing::warn!("KILL SWITCH ACTIVATED via command");

                // Stop all loops
                for wid in &trade_up_wallets {
                    let _ = self.trade_up.stop_loop(*wid).await;
                }
                for wid in &yield_wallets {
                    let _ = self.yield_orch.stop_loop(*wid).await;
                }

                CommandReport {
                    strategy: "Kill".into(),
                    action_taken: "all_loops_killed".into(),
                    params_used: params,
                    reasoning: intent.reasoning,
                }
            }
            "Resume" => {
                self.kill_flag.store(false, Ordering::SeqCst);
                tracing::info!("KILL SWITCH DEACTIVATED via command");

                CommandReport {
                    strategy: "Resume".into(),
                    action_taken: "kill_flag_cleared".into(),
                    params_used: params,
                    reasoning: intent.reasoning,
                }
            }
            "Status" | _ => {
                CommandReport {
                    strategy: "Status".into(),
                    action_taken: "status_report".into(),
                    params_used: serde_json::json!({
                        "active_loops": active_loops,
                        "kill_flag": self.kill_flag.load(Ordering::SeqCst),
                        "chains": self.config.chains,
                        "trade_up_wallets": trade_up_wallets.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                        "yield_wallets": yield_wallets.iter().map(|id| id.to_string()).collect::<Vec<_>>(),
                    }),
                    reasoning: intent.reasoning,
                }
            }
        }
    }
}
