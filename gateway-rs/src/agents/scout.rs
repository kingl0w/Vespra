use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use crate::agents::AgentClient;
use crate::config::GatewayConfig;
use crate::data::yield_provider::ProviderRegistry;
use crate::types::decisions::ScoutDecision;
use crate::types::opportunity::{EntrySignal, Opportunity, RiskTier};

#[derive(Debug, Clone, Serialize)]
pub struct ScoutContext {
    pub wallet_id: uuid::Uuid,
    pub mode: String,
    pub pools: Vec<Opportunity>,
    pub chains: Vec<String>,
}

const SYSTEM_PROMPT: &str = "You are Scout, market intelligence agent of the Vespra DeFi swarm. \
You MUST respond with valid JSON only. No prose, no markdown. Base your analysis on LIVE_POOL_DATA.\n\n\
Output schema: { \"opportunities\": [ { \"protocol\": \"string\", \"pool_id\": \"string\", \
\"chain\": \"string\", \"symbol\": \"string\", \"apy\": float, \"tvl_usd\": float, \
\"risk_tier\": \"LOW|MEDIUM|HIGH\", \"recommended_action\": \"string\" } ], \
\"summary\": \"string\", \"top_chain\": \"string\", \"strong_signal_count\": int }\n\n\
Rules: No transactions, no keys. Analyze LIVE_POOL_DATA only. \
Return max 5 opportunities sorted by apy descending.";

/// Symbols Scout is allowed to recommend on testnet chains. Anything else
/// has no real ERC-20 contract on Base Sepolia and is not swappable.
const TESTNET_ALLOWED_SYMBOLS: &[&str] = &["WETH", "ETH", "USDC", "USDBC", "DAI", "WBTC"];

const TESTNET_PROMPT_SUFFIX: &str = "\n\n\
TESTNET CONSTRAINT: At least one of the chains in this request is a testnet \
(name contains 'sepolia' or 'testnet'). On testnet you MUST only recommend pools \
where BOTH tokens in the pair are one of: WETH, ETH, USDC, USDBC, DAI, WBTC. \
Ignore every other pool regardless of how high the APY looks — most testnet pools \
contain fake tokens with no real ERC-20 contract and no liquidity, so they cannot \
be swapped. If no pool in LIVE_POOL_DATA passes this filter, return an empty \
opportunities array rather than recommending unswappable pools.";

fn chains_include_testnet(chains: &[String]) -> bool {
    chains.iter().any(|c| {
        let l = c.to_lowercase();
        l.contains("sepolia") || l.contains("testnet") || l.contains("goerli")
    })
}

/// VES-109: parse a pool pair symbol into its two token symbols, regardless
/// of which delimiter the upstream data source used. Handles `-`, `/`, `//`,
/// `_`, and `:` (and any combination of them — `//` falls out of splitting on
/// `/` once and dropping empty fragments). Whitespace is trimmed off each
/// half. Returns `Err` when the input doesn't yield exactly 2 non-empty
/// fragments so callers can fail loudly instead of silently mis-classifying.
fn parse_pool_symbol(raw_symbol: &str) -> Result<(String, String)> {
    let parts: Vec<String> = raw_symbol
        .split(|c: char| matches!(c, '-' | '/' | '_' | ':'))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 2 {
        anyhow::bail!("could not parse token pair from pool symbol: {raw_symbol}");
    }
    let mut iter = parts.into_iter();
    let a = iter.next().unwrap();
    let b = iter.next().unwrap();
    Ok((a, b))
}

/// Check whether a pool symbol like "WETH-USDC" or "USDC-VFY" only references
/// tokens in the testnet allowlist. Pools whose symbol can't be parsed into
/// exactly two tokens are rejected (we can't tell what they contain).
fn pool_symbol_is_testnet_safe(symbol: &str) -> bool {
    let (a, b) = match parse_pool_symbol(symbol) {
        Ok(pair) => pair,
        Err(_) => return false,
    };
    [a, b].iter().all(|p| {
        let upper = p.to_uppercase();
        TESTNET_ALLOWED_SYMBOLS.iter().any(|allowed| *allowed == upper)
    })
}

/// Expected shape of each opportunity in the LLM response.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
struct ScoutOutputOpportunity {
    protocol: Option<String>,
    pool_id: Option<String>,
    // Accept either pool_id or pool for backwards compat
    pool: Option<String>,
    chain: Option<String>,
    symbol: Option<String>,
    apy: Option<f64>,
    tvl_usd: Option<f64>,
    risk_tier: Option<String>,
    recommended_action: Option<String>,
    // Legacy fields — tolerate but don't require
    momentum_score: Option<f64>,
    entry_signal: Option<String>,
    price_change_24h_pct: Option<f64>,
}

#[derive(serde::Deserialize)]
struct ScoutOutput {
    opportunities: Vec<ScoutOutputOpportunity>,
}

pub struct ScoutAgent {
    llm: Arc<dyn AgentClient>,
    yield_registry: Option<Arc<ProviderRegistry>>,
    config: Option<Arc<GatewayConfig>>,
}

impl ScoutAgent {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self {
            llm,
            yield_registry: None,
            config: None,
        }
    }

    /// Attach a yield provider registry so Scout can fetch live pool data.
    pub fn with_yield_registry(
        mut self,
        registry: Arc<ProviderRegistry>,
        config: Arc<GatewayConfig>,
    ) -> Self {
        self.yield_registry = Some(registry);
        self.config = Some(config);
        self
    }

    /// Build a compact yield context block from live provider data.
    async fn build_yield_context(&self, chains: &[String]) -> Option<String> {
        let registry = self.yield_registry.as_ref()?;
        let config = self.config.as_ref()?;

        let chain_filter = if chains.len() == 1 {
            Some(chains[0].as_str())
        } else {
            None
        };

        let pools = match registry
            .fetch_pools(
                chain_filter,
                config.yield_min_tvl_usd,
                config.yield_min_apy,
            )
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("scout: yield provider fetch failed: {e}");
                return None;
            }
        };

        if pools.is_empty() {
            return None;
        }

        let top_n = config.yield_top_n;
        let truncated: Vec<_> = pools.into_iter().take(top_n).collect();

        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
        let mut lines = vec![format!("Live yield data (as of {timestamp}):")];
        for p in &truncated {
            let apy_7d = p
                .apy_7d
                .map(|v| format!("{v:.2}%"))
                .unwrap_or_else(|| "n/a".into());
            lines.push(format!(
                "{} | {} | {} | APY: {:.2}% | TVL: ${:.0} | 7d: {}",
                p.protocol, p.chain, p.symbol, p.apy, p.tvl_usd, apy_7d
            ));
        }

        Some(lines.join("\n"))
    }

    pub async fn analyze(&self, ctx: &ScoutContext) -> Result<ScoutDecision> {
        let pools_json = serde_json::to_string(&ctx.pools)?;

        // Build live yield context if registry is available
        let yield_context = self.build_yield_context(&ctx.chains).await;

        let testnet_run = chains_include_testnet(&ctx.chains);

        let mut system = match &yield_context {
            Some(yc) => format!("{SYSTEM_PROMPT}\n\n{yc}"),
            None => SYSTEM_PROMPT.to_string(),
        };
        if testnet_run {
            system.push_str(TESTNET_PROMPT_SUFFIX);
        }

        let task = format!(
            "LIVE_POOL_DATA: {pools_json}\n\n\
             [TASK] Find momentum opportunities for wallet {} mode={}",
            ctx.wallet_id, ctx.mode
        );

        let raw = self.llm.call(&system, &task).await?;

        // Validate against expected schema
        let opps = match serde_json::from_str::<ScoutOutput>(&raw) {
            Ok(output) => {
                output
                    .opportunities
                    .into_iter()
                    .filter_map(|item| convert_scout_opportunity(item))
                    .collect::<Vec<_>>()
            }
            Err(_) => {
                // Fallback: try parsing as Value for backwards compat
                match serde_json::from_str::<serde_json::Value>(&raw) {
                    Ok(val) => {
                        let arr = if let Some(arr) = val.get("opportunities").and_then(|v| v.as_array()) {
                            arr.clone()
                        } else if val.is_array() {
                            val.as_array().cloned().unwrap_or_default()
                        } else {
                            tracing::warn!(
                                "scout: LLM output is not valid JSON matching expected schema. Raw: {}",
                                &raw[..raw.len().min(500)]
                            );
                            return Err(anyhow::anyhow!(
                                "Scout LLM output did not match expected schema — \
                                 expected {{\"opportunities\": [...]}} object"
                            ));
                        };
                        arr.iter()
                            .filter_map(|item| parse_opportunity(item))
                            .collect::<Vec<_>>()
                    }
                    Err(parse_err) => {
                        tracing::warn!(
                            "scout: LLM returned invalid JSON. Error: {parse_err}. Raw: {}",
                            &raw[..raw.len().min(500)]
                        );
                        return Err(anyhow::anyhow!(
                            "Scout LLM output is not valid JSON: {parse_err}"
                        ));
                    }
                }
            }
        };

        // Deterministic safety net: on testnet runs, drop any opportunity whose
        // pool symbol references tokens outside the testnet allowlist. This is
        // belt-and-braces — the prompt asks the LLM not to surface these, but
        // LLMs are unreliable and the goal runner will otherwise loop forever
        // re-scouting unswappable pools.
        let opps: Vec<_> = if testnet_run {
            let before = opps.len();
            let filtered: Vec<_> = opps
                .into_iter()
                .filter(|o| {
                    let safe = pool_symbol_is_testnet_safe(&o.pool);
                    if !safe {
                        tracing::info!(
                            "[scout] testnet filter dropped pool '{}' (protocol={}) — contains unknown tokens",
                            o.pool, o.protocol
                        );
                    }
                    safe
                })
                .collect();
            tracing::info!(
                "[scout] testnet filter: {} → {} opportunities",
                before,
                filtered.len()
            );
            filtered
        } else {
            opps
        };

        if opps.is_empty() {
            Ok(ScoutDecision::NoOpportunities {
                reason: if testnet_run {
                    "no testnet-swappable pools (only WETH/USDC/USDBC/DAI/WBTC pairs allowed)".into()
                } else {
                    "no valid opportunities in LLM response".into()
                },
            })
        } else {
            Ok(ScoutDecision::Opportunities(opps))
        }
    }

    pub async fn query(&self, question: &str) -> Result<String> {
        let prompt = format!("{}\n\nHowever, for this request respond with helpful prose or JSON as appropriate. \
            Do not restrict yourself to the opportunities schema — answer the user's question directly.", SYSTEM_PROMPT);
        self.llm.call(&prompt, question).await
    }
}

/// Convert a typed ScoutOutputOpportunity into an Opportunity.
fn convert_scout_opportunity(item: ScoutOutputOpportunity) -> Option<Opportunity> {
    let protocol = item.protocol?;
    if protocol.is_empty() {
        return None;
    }
    let pool = item
        .pool_id
        .or(item.pool)
        .unwrap_or_default();
    let chain = item.chain.unwrap_or_default();
    let apy = item.apy.unwrap_or(0.0);
    let tvl_usd = item.tvl_usd.unwrap_or(0.0) as u64;

    let momentum_score = item.momentum_score.unwrap_or(0.0);
    let price_change_24h_pct = item.price_change_24h_pct.unwrap_or(0.0);

    let entry_signal = match item
        .entry_signal
        .as_deref()
        .unwrap_or("none")
        .to_lowercase()
        .as_str()
    {
        "strong" => EntrySignal::Strong,
        "moderate" => EntrySignal::Moderate,
        "weak" => EntrySignal::Weak,
        _ => EntrySignal::None,
    };

    let risk_tier = match item
        .risk_tier
        .as_deref()
        .unwrap_or("HIGH")
        .to_uppercase()
        .as_str()
    {
        "LOW" => RiskTier::Low,
        "MEDIUM" => RiskTier::Medium,
        _ => RiskTier::High,
    };

    Some(Opportunity {
        protocol,
        pool,
        chain,
        apy,
        tvl_usd,
        momentum_score,
        entry_signal,
        price_change_24h_pct,
        price_usd: 0.0,
        risk_tier,
        il_risk: false,
        volume_24h: 0,
        volume_spike_pct: 0.0,
        tvl_change_7d_pct: 0.0,
    })
}

/// Parse an Opportunity from a serde_json::Value, using defaults for missing fields.
fn parse_opportunity(item: &serde_json::Value) -> Option<Opportunity> {
    let protocol = item.get("protocol")?.as_str()?.to_string();
    let pool = item
        .get("pool_id")
        .or_else(|| item.get("pool"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let chain = item
        .get("chain")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let apy = item.get("apy").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let tvl_usd = item
        .get("tvl_usd")
        .and_then(|v| v.as_f64().map(|f| f as u64).or_else(|| v.as_u64()))
        .unwrap_or(0);
    let momentum_score = item
        .get("momentum_score")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let price_change_24h_pct = item
        .get("price_change_24h_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let price_usd = item
        .get("price_usd")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let volume_24h = item
        .get("volume_24h")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let volume_spike_pct = item
        .get("volume_spike_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let tvl_change_7d_pct = item
        .get("tvl_change_7d_pct")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let il_risk = item
        .get("il_risk")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let entry_signal = match item
        .get("entry_signal")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_lowercase()
        .as_str()
    {
        "strong" => EntrySignal::Strong,
        "moderate" => EntrySignal::Moderate,
        "weak" => EntrySignal::Weak,
        _ => EntrySignal::None,
    };

    let risk_tier = match item
        .get("risk_tier")
        .and_then(|v| v.as_str())
        .unwrap_or("HIGH")
        .to_uppercase()
        .as_str()
    {
        "LOW" => RiskTier::Low,
        "MEDIUM" => RiskTier::Medium,
        _ => RiskTier::High,
    };

    Some(Opportunity {
        protocol,
        pool,
        chain,
        apy,
        tvl_usd,
        momentum_score,
        entry_signal,
        price_change_24h_pct,
        price_usd,
        risk_tier,
        il_risk,
        volume_24h,
        volume_spike_pct,
        tvl_change_7d_pct,
    })
}
