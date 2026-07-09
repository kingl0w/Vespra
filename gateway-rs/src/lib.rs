pub mod agent_config;
pub mod agents;
pub mod backtest;
pub mod chain;
pub mod config;
pub mod dag_registry;
pub mod data;
pub mod error;
pub mod execution_gate;
pub mod fee;
pub mod fees;
pub mod goal_runner;
pub mod guards;
pub mod middleware;
pub mod orchestrator;
pub mod routes;
pub mod sentinel_monitor;
pub mod types;
pub mod yield_scheduler;

/// Truncate a string to at most `max` chars on a char boundary. LLM/HTTP
/// responses contain multi-byte UTF-8; byte-slicing them (`&s[..n]`) panics
/// when `n` lands mid-codepoint. Use this for log/error previews.
pub fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

/// Constant-time byte comparison for secrets (bearer tokens), so a wrong token
/// can't be recovered via response timing. `black_box` stops the optimizer
/// short-circuiting the fold. Length is not treated as secret.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_semantics() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_eq(b"secret-token", b"secret-tokeX"));
        assert!(!constant_time_eq(b"short", b"longer-token"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn truncate_chars_never_splits_codepoints() {
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abc", 10), "abc");
        // 4-byte emoji: truncating at 1 char keeps it whole, never panics
        assert_eq!(truncate_chars("🚀🚀🚀", 1), "🚀");
    }
}
