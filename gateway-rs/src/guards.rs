use crate::config::GatewayConfig;

pub fn slippage_ok(actual_pct: f64, config: &GatewayConfig) -> bool {
    if actual_pct > config.trader_max_slippage_pct {
        tracing::warn!(
            "slippage guard: {:.2}% > {:.2}% — swap aborted",
            actual_pct,
            config.trader_max_slippage_pct
        );
        false
    } else {
        true
    }
}

pub fn tx_deadline(config: &GatewayConfig) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now + config.trade_up_cycle_interval_secs
}
