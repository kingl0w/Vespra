
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, Utc};
use uuid::Uuid;

use crate::agents::AgentClient;
use crate::backtest::types::{
    BacktestMode, BacktestRequest, BacktestResult, EquityPoint,
};
use crate::data::historical::{ApySnapshot, HistoricalFeed, PriceSnapshot};

const DEFAULT_CAPITAL_ETH: f64 = 1.0;
const DEFAULT_MIN_APY_PCT: f64 = 5.0;
const APY_EXIT_THRESHOLD_PCT: f64 = 2.0;
const PRICE_DRAWDOWN_EXIT_PCT: f64 = 5.0;
const FEE_PER_TRADE_ETH: f64 = 0.0001;

struct DayBar {
    date: NaiveDate,
    apy: f64,
    price: f64,
}

///tracks the state of a hypothetical position across the simulation.
struct OpenPosition {
    entry_date: NaiveDate,
    entry_price: f64,
    entry_capital: f64,
    peak_price: f64,
}

struct TradeOutcome {
    ///net p&l on the trade as a fraction (e.g. 0.04 for +4%).
    pnl_frac: f64,
}

pub async fn run_backtest(
    request: &BacktestRequest,
    feed: Arc<dyn HistoricalFeed>,
    llm: Arc<dyn AgentClient>,
) -> Result<BacktestResult> {
    if request.from_date > request.to_date {
        anyhow::bail!(
            "from_date {} is after to_date {}",
            request.from_date,
            request.to_date
        );
    }

    let pool_id = request
        .pool_id
        .clone()
        .unwrap_or_else(|| "aa70268e-4b52-42bf-a116-608b370f9501".to_string());
    let coingecko_id = request
        .coingecko_id
        .clone()
        .unwrap_or_else(|| "ethereum".to_string());

    let apy_series: Vec<ApySnapshot> = feed
        .apy_series(&pool_id, request.from_date, request.to_date)
        .await
        .with_context(|| format!("apy_series fetch failed for pool {pool_id}"))?;
    let price_series: Vec<PriceSnapshot> = feed
        .price_series(&coingecko_id, request.from_date, request.to_date)
        .await
        .with_context(|| format!("price_series fetch failed for coin {coingecko_id}"))?;

    let bars = zip_series(apy_series, price_series);
    if bars.is_empty() {
        tracing::warn!(
            "[backtest] no overlapping APY/price data for {}..{} (pool={}, coin={})",
            request.from_date,
            request.to_date,
            pool_id,
            coingecko_id
        );
    }

    let result = match request.mode {
        BacktestMode::Rules => run_rules_mode(request, &bars),
        BacktestMode::Agents => run_agents_mode(request, &bars, llm).await?,
    };

    Ok(result)
}

//─── series alignment ──────────────────────────────────────────────────

///pair apy and price by date. days missing on either side are dropped so the
///simulation never compounds against half-known state.
fn zip_series(apy: Vec<ApySnapshot>, price: Vec<PriceSnapshot>) -> Vec<DayBar> {
    let apy_map: BTreeMap<NaiveDate, f64> =
        apy.into_iter().map(|s| (s.date, s.apy)).collect();
    let price_map: BTreeMap<NaiveDate, f64> =
        price.into_iter().map(|s| (s.date, s.price_usd)).collect();

    let mut out = Vec::new();
    for (date, apy) in &apy_map {
        if let Some(price) = price_map.get(date) {
            out.push(DayBar {
                date: *date,
                apy: *apy,
                price: *price,
            });
        }
    }
    out
}

//─── shared bookkeeping ────────────────────────────────────────────────

///greatest peak-to-trough drawdown across the equity curve, expressed as a
///percentage. returns 0.0 for an empty or monotonically increasing curve.
fn max_drawdown_pct(curve: &[EquityPoint]) -> f64 {
    let mut peak = f64::MIN;
    let mut max_dd = 0.0_f64;
    for point in curve {
        if point.value_eth > peak {
            peak = point.value_eth;
        }
        if peak > 0.0 {
            let dd = (peak - point.value_eth) / peak * 100.0;
            if dd > max_dd {
                max_dd = dd;
            }
        }
    }
    max_dd
}

fn build_result(
    request: &BacktestRequest,
    mode: BacktestMode,
    strategy_summary: String,
    capital_start: f64,
    capital_end: f64,
    total_trades: u32,
    wins: u32,
    equity_curve: Vec<EquityPoint>,
) -> BacktestResult {
    let pnl_eth = capital_end - capital_start;
    let pnl_pct = if capital_start > 0.0 {
        (pnl_eth / capital_start) * 100.0
    } else {
        0.0
    };
    let win_rate_pct = if total_trades > 0 {
        (wins as f64 / total_trades as f64) * 100.0
    } else {
        0.0
    };
    let max_dd = max_drawdown_pct(&equity_curve);

    BacktestResult {
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        mode,
        strategy_summary,
        period_from: request.from_date,
        period_to: request.to_date,
        pnl_pct,
        pnl_eth,
        max_drawdown_pct: max_dd,
        win_rate_pct,
        total_trades,
        fee_estimate_eth: total_trades as f64 * FEE_PER_TRADE_ETH,
        equity_curve,
    }
}

//─── rule-based mode ───────────────────────────────────────────────────

fn run_rules_mode(request: &BacktestRequest, bars: &[DayBar]) -> BacktestResult {
    let min_apy = std::env::var("BACKTEST_MIN_APY_PCT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(DEFAULT_MIN_APY_PCT);

    let mut capital = DEFAULT_CAPITAL_ETH;
    let mut equity_curve = Vec::with_capacity(bars.len());
    let mut open: Option<OpenPosition> = None;
    let mut total_trades = 0u32;
    let mut wins = 0u32;

    for bar in bars {
        //compound while a position is open: daily apy accrual.
        if let Some(pos) = open.as_mut() {
            let daily_yield = (bar.apy / 100.0) / 365.0;
            capital *= 1.0 + daily_yield;
            if bar.price > pos.peak_price {
                pos.peak_price = bar.price;
            }
        }

        //check exit conditions on an open position.
        let mut should_exit = false;
        if let Some(pos) = open.as_ref() {
            let drawdown_pct =
                ((pos.peak_price - bar.price) / pos.peak_price.max(f64::EPSILON)) * 100.0;
            if drawdown_pct >= PRICE_DRAWDOWN_EXIT_PCT || bar.apy < APY_EXIT_THRESHOLD_PCT {
                should_exit = true;
            }
        }

        if should_exit {
            if let Some(pos) = open.take() {
                let outcome = TradeOutcome {
                    pnl_frac: (capital - pos.entry_capital) / pos.entry_capital,
                };
                total_trades += 1;
                if outcome.pnl_frac > 0.0 {
                    wins += 1;
                }
            }
        }

        //entry signal — only when flat.
        if open.is_none() && bar.apy >= min_apy {
            open = Some(OpenPosition {
                entry_date: bar.date,
                entry_price: bar.price,
                entry_capital: capital,
                peak_price: bar.price,
            });
        }

        equity_curve.push(EquityPoint {
            date: bar.date,
            value_eth: capital,
        });
    }

    //force-close any still-open trade at the final bar so the win-rate
    //and trade count reflect the full simulation.
    if let Some(pos) = open.take() {
        let outcome = TradeOutcome {
            pnl_frac: (capital - pos.entry_capital) / pos.entry_capital,
        };
        total_trades += 1;
        if outcome.pnl_frac > 0.0 {
            wins += 1;
        }
        let _ = pos.entry_date; // silence unused warning
        let _ = pos.entry_price;
    }

    let summary = format!(
        "rules: min_apy={min_apy:.1}%, exit when drawdown>={PRICE_DRAWDOWN_EXIT_PCT:.0}% or apy<{APY_EXIT_THRESHOLD_PCT:.0}%"
    );

    build_result(
        request,
        BacktestMode::Rules,
        summary,
        DEFAULT_CAPITAL_ETH,
        capital,
        total_trades,
        wins,
        equity_curve,
    )
}

//─── agent-based mode ──────────────────────────────────────────────────

async fn run_agents_mode(
    request: &BacktestRequest,
    bars: &[DayBar],
    llm: Arc<dyn AgentClient>,
) -> Result<BacktestResult> {
    let estimated_calls = bars.len() as u64 * 4;
    tracing::warn!(
        "agent-based backtest will consume LLM credits — {} days × 4 agents = {} LLM calls estimated",
        bars.len(),
        estimated_calls
    );

    let mut capital = DEFAULT_CAPITAL_ETH;
    let mut equity_curve = Vec::with_capacity(bars.len());
    let mut open: Option<OpenPosition> = None;
    let mut total_trades = 0u32;
    let mut wins = 0u32;

    for bar in bars {
        let date_str = bar.date.format("%Y-%m-%d").to_string();
        let date_prefix = format!(
            "You are evaluating HISTORICAL data for a backtest. Date: {date_str}. \
             Do not assume current market conditions."
        );

        //compound any open position by the daily apy accrual.
        if let Some(pos) = open.as_mut() {
            let daily_yield = (bar.apy / 100.0) / 365.0;
            capital *= 1.0 + daily_yield;
            if bar.price > pos.peak_price {
                pos.peak_price = bar.price;
            }
        }

        //── scout: should we be looking at this pool today? ──────────
        let scout_system = format!(
            "{date_prefix}\n\nYou are Scout. Respond with JSON only: \
             {{\"signal\": \"enter\"|\"skip\", \"reason\": \"<short>\"}}"
        );
        let scout_task = format!(
            "Pool {} on chain {} — historical APY today: {:.2}%, price: ${:.2}. \
             Goal: {}",
            request.pool_id.as_deref().unwrap_or("default"),
            request.chain,
            bar.apy,
            bar.price,
            request.raw_goal
        );
        let scout_raw = llm
            .call(&scout_system, &scout_task)
            .await
            .unwrap_or_else(|_| "{\"signal\":\"skip\"}".to_string());
        let scout_signal = parse_signal(&scout_raw, "signal").unwrap_or_else(|| "skip".into());

        //── risk: gate the entry on a synthetic risk read. ───────────
        let risk_system = format!(
            "{date_prefix}\n\nYou are Risk. Respond with JSON only: \
             {{\"gate_pass\": true|false, \"score\": \"LOW|MEDIUM|HIGH|CRITICAL\"}}"
        );
        let risk_task = format!(
            "Evaluate gate for pool on chain {}. APY={:.2}%, price=${:.2}. \
             Be conservative on mainnet, lenient on testnet.",
            request.chain, bar.apy, bar.price
        );
        let risk_raw = llm
            .call(&risk_system, &risk_task)
            .await
            .unwrap_or_else(|_| "{\"gate_pass\":false,\"score\":\"HIGH\"}".to_string());
        let risk_pass = parse_bool(&risk_raw, "gate_pass").unwrap_or(false);

        //── trader: only consulted while flat and gates are open. ────
        let mut trader_action = "hold".to_string();
        if open.is_none() && scout_signal == "enter" && risk_pass {
            let trader_system = format!(
                "{date_prefix}\n\nYou are Trader. Respond with JSON only: \
                 {{\"action\": \"swap\"|\"hold\", \"reasoning\": \"<short>\"}}"
            );
            let trader_task = format!(
                "Scout flagged entry. Risk gate passed. APY={:.2}%, price=${:.2}, \
                 capital_eth={capital:.4}. Decide swap or hold for the historical date.",
                bar.apy, bar.price
            );
            let trader_raw = llm
                .call(&trader_system, &trader_task)
                .await
                .unwrap_or_else(|_| "{\"action\":\"hold\"}".to_string());
            trader_action = parse_signal(&trader_raw, "action").unwrap_or_else(|| "hold".into());
        }

        //apply trader's swap decision (entry).
        if open.is_none() && trader_action == "swap" {
            open = Some(OpenPosition {
                entry_date: bar.date,
                entry_price: bar.price,
                entry_capital: capital,
                peak_price: bar.price,
            });
        }

        //── sentinel: only when a position is open. ──────────────────
        if open.is_some() {
            let pos_ref = open.as_ref().unwrap();
            let gain_pct = ((bar.price - pos_ref.entry_price) / pos_ref.entry_price) * 100.0;
            let sentinel_system = format!(
                "{date_prefix}\n\nYou are Sentinel. Respond with JSON only: \
                 {{\"action\": \"hold\"|\"exit_gain\"|\"exit_loss\"}}"
            );
            let sentinel_task = format!(
                "Open position entered at ${:.2}, current ${:.2}, gain/loss {:.2}%. \
                 APY today {:.2}%. Decide hold/exit_gain/exit_loss.",
                pos_ref.entry_price, bar.price, gain_pct, bar.apy
            );
            let sentinel_raw = llm
                .call(&sentinel_system, &sentinel_task)
                .await
                .unwrap_or_else(|_| "{\"action\":\"hold\"}".to_string());
            let sentinel_action =
                parse_signal(&sentinel_raw, "action").unwrap_or_else(|| "hold".into());

            if sentinel_action == "exit_gain" || sentinel_action == "exit_loss" {
                if let Some(pos) = open.take() {
                    let pnl_frac = (capital - pos.entry_capital) / pos.entry_capital;
                    total_trades += 1;
                    if pnl_frac > 0.0 {
                        wins += 1;
                    }
                }
            }
        }

        equity_curve.push(EquityPoint {
            date: bar.date,
            value_eth: capital,
        });
    }

    //force-close at end of period.
    if let Some(pos) = open.take() {
        let pnl_frac = (capital - pos.entry_capital) / pos.entry_capital;
        total_trades += 1;
        if pnl_frac > 0.0 {
            wins += 1;
        }
    }

    let summary = format!(
        "agents: Scout/Risk/Trader/Sentinel run against historical APY+price for {} days",
        bars.len()
    );

    Ok(build_result(
        request,
        BacktestMode::Agents,
        summary,
        DEFAULT_CAPITAL_ETH,
        capital,
        total_trades,
        wins,
        equity_curve,
    ))
}


fn parse_signal(raw: &str, key: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get(key)
        .and_then(|s| s.as_str())
        .map(|s| s.trim().to_lowercase())
}

fn parse_bool(raw: &str, key: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get(key).and_then(|b| b.as_bool())
}

//silence unused warnings on internal duration helper kept for future use.
#[allow(dead_code)]
fn day_count(from: NaiveDate, to: NaiveDate) -> i64 {
    (to - from).num_days() + 1
}

#[allow(dead_code)]
fn _force_duration_use() -> Duration {
    Duration::days(1)
}
