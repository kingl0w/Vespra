use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::config::GatewayConfig;
use crate::data::aave::{AaveFetcher, AavePosition};
use crate::data::yield_provider::ProviderRegistry;
use crate::types::decisions::YieldDecision;

//── context types (kept for backwards compat with orchestrator) ──

#[derive(Debug, Clone, Serialize)]
pub struct YieldContext {
    pub current_position: Option<CurrentPosition>,
    pub candidates: Vec<YieldCandidate>,
    pub threshold_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentPosition {
    pub protocol: String,
    pub apy_pct: f64,
    pub amount_eth: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct YieldCandidate {
    pub protocol: String,
    pub pool_id: String,
    pub apy_pct: f64,
    pub chain: String,
    pub tvl_usd: u64,
    pub momentum_score: f64,
}

//── live analysis result ─────────────────────────────────────────

///structured output from the full yield analysis pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YieldAnalysis {
    pub positions: Vec<AavePosition>,
    pub recommended_action: String,
    pub target_protocol: String,
    pub target_asset: String,
    pub amount_eth: f64,
    pub executor_ready: bool,
    pub reasoning: String,
}

///expected llm output schema for validation.
#[derive(Deserialize)]
#[allow(dead_code)]
struct YieldLlmOutput {
    recommended_action: Option<String>,
    target_protocol: Option<String>,
    target_asset: Option<String>,
    amount_eth: Option<f64>,
    executor_ready: Option<bool>,
    reasoning: Option<String>,
}

//── system prompt ────────────────────────────────────────────────

const SYSTEM_PROMPT: &str = "You are Yield, the yield rotation specialist of the Vespra DeFi agent swarm. \
You MUST respond with valid JSON only. No prose, no markdown.\n\n\
Compare the current yield position against candidate pools. Recommend rebalancing \
only when a candidate offers a materially higher APY above the threshold.\n\n\
Output schema: { \"recommended_action\": \"deposit\" | \"withdraw\" | \"rebalance\" | \"hold\", \
\"target_protocol\": \"string\", \"target_asset\": \"string\", \
\"amount_eth\": float, \"executor_ready\": bool, \"reasoning\": \"string\" }\n\n\
Rules:\n\
- Only recommend rebalance if target APY > best current net_apy + 0.5% (after gas drag)\n\
- If no current position and good opportunities exist, recommend deposit\n\
- If health_factor < 1.1, recommend withdraw to safety\n\
- Consider TVL, protocol reputation, and gas costs when deciding\n\
- Be conservative: gas costs and slippage erode small gains\n\
- executor_ready = true only for deposit/rebalance with clear parameters";

const DELTA_THRESHOLD: f64 = 0.5; // 0.5% APY improvement required

//── agent ────────────────────────────────────────────────────────

pub struct YieldAgent {
    llm: Arc<dyn AgentClient>,
    aave_fetcher: Option<Arc<AaveFetcher>>,
    yield_registry: Option<Arc<ProviderRegistry>>,
    config: Option<Arc<GatewayConfig>>,
}

impl YieldAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self {
            llm,
            aave_fetcher: None,
            yield_registry: None,
            config: None,
        }
    }

    ///attach live data sources for real position analysis.
    pub fn with_live_data(
        mut self,
        aave_fetcher: Arc<AaveFetcher>,
        yield_registry: Arc<ProviderRegistry>,
        config: Arc<GatewayConfig>,
    ) -> Self {
        self.aave_fetcher = Some(aave_fetcher);
        self.yield_registry = Some(yield_registry);
        self.config = Some(config);
        self
    }

    ///legacy evaluate path used by the yield orchestrator.
    pub async fn evaluate(&self, ctx: &YieldContext) -> Result<YieldDecision> {
        let ctx_json = serde_json::to_string(ctx)?;
        let task = format!(
            "YIELD_CONTEXT: {ctx_json}\n\n\
             [TASK] Evaluate whether to rotate yield position. Threshold = {:.2}%",
            ctx.threshold_pct
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        let val: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        let action = val
            .get("recommended_action")
            .or_else(|| val.get("action"))
            .and_then(|v| v.as_str())
            .unwrap_or("hold");

        if action.eq_ignore_ascii_case("rebalance") || action.eq_ignore_ascii_case("deposit") {
            Ok(YieldDecision::Rebalance {
                target_protocol: val
                    .get("target_protocol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                target_pool_id: val
                    .get("target_pool_id")
                    .or_else(|| val.get("target_asset"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                expected_gain_pct: val
                    .get("expected_apy_gain_pct")
                    .or_else(|| val.get("amount_eth"))
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0),
                reasoning: val
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("rebalance recommended")
                    .to_string(),
            })
        } else {
            Ok(YieldDecision::Hold {
                reasoning: val
                    .get("reasoning")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hold — no better opportunity")
                    .to_string(),
            })
        }
    }

    ///full live analysis: fetch aave positions, compare with scout opportunities,
    ///and produce a structured recommendation.
    pub async fn analyze_live(
        &self,
        chain: &str,
        wallet_address: &str,
    ) -> Result<YieldAnalysis> {
        let aave = self
            .aave_fetcher
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("yield agent: aave fetcher not configured"))?;
        let registry = self
            .yield_registry
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("yield agent: provider registry not configured"))?;
        let config = self
            .config
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("yield agent: config not configured"))?;

        //1. fetch current aave positions
        let positions = aave.fetch_positions(chain, wallet_address).await?;

        //2. fetch top scout opportunities from providerregistry
        let opportunities = registry
            .fetch_pools(Some(chain), config.yield_min_tvl_usd, config.yield_min_apy)
            .await
            .unwrap_or_default();

        let top_5: Vec<_> = opportunities.into_iter().take(5).collect();

        //3. find the best current position net apy (after gas drag)
        let best_current_net = positions
            .iter()
            .map(|p| p.net_apy - p.gas_drag_apy)
            .fold(f64::NEG_INFINITY, f64::max);

        //4. check if any opportunity beats current by the delta threshold
        let best_opportunity = top_5
            .iter()
            .find(|o| o.apy > best_current_net + DELTA_THRESHOLD);

        //5. build prompt context
        let positions_summary = if positions.is_empty() {
            "No current Aave V3 positions.".to_string()
        } else {
            positions
                .iter()
                .map(|p| {
                    format!(
                        "  {} | supplied: {:.4} | borrowed: {:.4} | supply_apy: {:.2}% | borrow_apy: {:.2}% | net_apy: {:.2}% | gas_drag: {:.2}% | hf: {}",
                        p.asset, p.supplied, p.borrowed, p.supply_apy, p.borrow_apy, p.net_apy, p.gas_drag_apy,
                        p.health_factor.map(|h| format!("{h:.2}")).unwrap_or_else(|| "n/a".into())
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let opportunities_summary = if top_5.is_empty() {
            "No yield opportunities found.".to_string()
        } else {
            top_5
                .iter()
                .map(|o| {
                    format!(
                        "  {} | {} | {} | APY: {:.2}% | TVL: ${:.0}",
                        o.protocol, o.chain, o.symbol, o.apy, o.tvl_usd
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let has_better = best_opportunity.is_some();

        let task = format!(
            "CURRENT AAVE V3 POSITIONS (chain={chain}, wallet={wallet_address}):\n\
             {positions_summary}\n\n\
             Best current net APY (after gas): {best_current_net:.2}%\n\n\
             TOP YIELD OPPORTUNITIES:\n\
             {opportunities_summary}\n\n\
             Delta threshold: {DELTA_THRESHOLD:.1}%\n\
             Better opportunity available: {has_better}\n\n\
             [TASK] Recommend: deposit | withdraw | rebalance | hold"
        );

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;

        //6. parse and validate llm output
        let analysis = match serde_json::from_str::<YieldLlmOutput>(&raw) {
            Ok(output) => YieldAnalysis {
                positions: positions.clone(),
                recommended_action: output
                    .recommended_action
                    .unwrap_or_else(|| "hold".into()),
                target_protocol: output.target_protocol.unwrap_or_default(),
                target_asset: output.target_asset.unwrap_or_default(),
                amount_eth: output.amount_eth.unwrap_or(0.0),
                executor_ready: output.executor_ready.unwrap_or(false),
                reasoning: output
                    .reasoning
                    .unwrap_or_else(|| "no reasoning provided".into()),
            },
            Err(_) => {
                //fallback: try parsing as generic value
                let val: serde_json::Value =
                    serde_json::from_str(&raw).map_err(|e| {
                        tracing::warn!(
                            "yield agent: LLM returned invalid JSON: {e}. Raw: {}",
                            &raw[..raw.len().min(500)]
                        );
                        anyhow::anyhow!("Yield LLM output is not valid JSON: {e}")
                    })?;

                let action = val
                    .get("recommended_action")
                    .or_else(|| val.get("action"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("hold");

                //validate action is one of the expected values
                let valid_actions = ["deposit", "withdraw", "rebalance", "hold"];
                let action_lower = action.to_lowercase();
                if !valid_actions.contains(&action_lower.as_str()) {
                    tracing::warn!(
                        "yield agent: unexpected action '{}' in LLM output, defaulting to hold. Raw: {}",
                        action,
                        &raw[..raw.len().min(500)]
                    );
                }

                YieldAnalysis {
                    positions: positions.clone(),
                    recommended_action: if valid_actions.contains(&action_lower.as_str()) {
                        action_lower
                    } else {
                        "hold".into()
                    },
                    target_protocol: val
                        .get("target_protocol")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    target_asset: val
                        .get("target_asset")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    amount_eth: val
                        .get("amount_eth")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0),
                    executor_ready: val
                        .get("executor_ready")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    reasoning: val
                        .get("reasoning")
                        .and_then(|v| v.as_str())
                        .unwrap_or("no reasoning provided")
                        .to_string(),
                }
            }
        };

        Ok(analysis)
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        let prompt = format!(
            "{}\n\nHowever, for this request respond with helpful prose or JSON as appropriate. \
             Do not restrict yourself to the yield schema — answer the user's question directly.",
            SYSTEM_PROMPT
        );
        self.llm.call(&prompt, question).await
    }
}
