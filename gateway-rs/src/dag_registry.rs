//! Named-DAG registry: expands a workflow name into a concrete nullboiler
//! `steps[]` payload. nullboiler has no named-workflow store of its own, so the
//! gateway owns these templates and sends the full workflow on spawn.
//!
//! Every step is analysis/planning only (LLM reasoning via the matching agent
//! worker) — NONE of these move funds. Fund movement stays in the audited
//! in-process goal pipeline (`goal_runner`), never through nullboiler.
//!
//! Template syntax is nullboiler's: `{{input.X}}` and `{{steps.ID.output}}`
//! (note the `steps.` prefix — bare `{{id.output}}` does NOT resolve).

use serde_json::{json, Value};

/// Build a nullboiler-compatible workflow (`{steps, input}`) for a known DAG
/// name, or `None` if the name isn't registered. `wallet`/`chain` are woven
/// into the run input so steps can reference `{{input.wallet}}` / `{{input.chain}}`.
pub fn workflow_for(name: &str, wallet: Option<&str>, chain: Option<&str>) -> Option<Value> {
    let steps = steps_for(name)?;
    Some(json!({
        "steps": steps,
        "input": {
            "dag": name,
            "wallet": wallet.unwrap_or(""),
            "chain": chain.unwrap_or(""),
        },
    }))
}

/// True if the given DAG name has a registered template.
pub fn is_registered(name: &str) -> bool {
    steps_for(name).is_some()
}

fn task(id: &str, tag: &str, prompt: &str, deps: &[&str]) -> Value {
    json!({
        "id": id,
        "type": "task",
        "worker_tags": [tag],
        "prompt_template": prompt,
        "depends_on": deps,
    })
}

fn steps_for(name: &str) -> Option<Value> {
    let steps = match name {
        // ── coordinator-spawned analysis pipelines ──────────────────────
        "yield_rotation" => json!([
            task("find", "scout",
                "Find the single best-yielding pool for wallet {{input.wallet}} on chain {{input.chain}}. \
                 Report pool, APY, TVL and why.", &[]),
            task("assess", "risk",
                "Assess the risk of this yield opportunity and give a LOW/MEDIUM/HIGH grade: {{steps.find.output}}",
                &["find"]),
            task("plan", "yield",
                "Given opportunity {{steps.find.output}} and risk {{steps.assess.output}}, produce a rotation \
                 PLAN (do not execute): amounts, from/to pool, expected gain. Output the plan only.",
                &["find", "assess"]),
        ]),
        "trade_up" => json!([
            task("find", "scout",
                "Find the best trade-up opportunity for wallet {{input.wallet}} on {{input.chain}}.", &[]),
            task("assess", "risk",
                "Grade the risk (LOW/MEDIUM/HIGH) of: {{steps.find.output}}", &["find"]),
            task("plan", "trader",
                "Decide enter/hold and produce a trade PLAN (do not execute) for {{steps.find.output}} \
                 given risk {{steps.assess.output}}.", &["find", "assess"]),
        ]),
        "rebalance" => json!([
            task("check", "sentinel",
                "Check the health of all open positions for wallet {{input.wallet}} on {{input.chain}}.", &[]),
            task("plan", "coordinator",
                "Given position health {{steps.check.output}}, propose a rebalance PLAN (do not execute): \
                 which positions to trim/add and why.", &["check"]),
        ]),

        // ── dashboard pipeline presets (analysis/planning) ──────────────
        "swap-with-risk-check" => json!([
            task("risk-assess", "risk",
                "Evaluate risk for swapping funds for wallet {{input.wallet}} on {{input.chain}} via Uniswap V3.", &[]),
            task("build-swap", "trader",
                "Build a swap PLAN (do not execute) given the risk assessment: {{steps.risk-assess.output}}",
                &["risk-assess"]),
            task("report", "coordinator",
                "Summarize the swap plan {{steps.build-swap.output}} and its risk {{steps.risk-assess.output}}.",
                &["build-swap", "risk-assess"]),
        ]),
        "yield-deposit" => json!([
            task("find-yield", "scout",
                "Find the best stablecoin yield for wallet {{input.wallet}} on {{input.chain}} right now.", &[]),
            task("assess-risk", "risk",
                "Evaluate risk for: {{steps.find-yield.output}}", &["find-yield"]),
            task("build-deposit", "yield",
                "Build a deposit PLAN (do not execute) for {{steps.find-yield.output}}.",
                &["find-yield", "assess-risk"]),
        ]),
        "health-monitor" => json!([
            task("check-positions", "sentinel",
                "Check all active positions for wallet {{input.wallet}} on {{input.chain}} for health warnings.", &[]),
            task("assess-alerts", "risk",
                "Evaluate these position alerts: {{steps.check-positions.output}}", &["check-positions"]),
            task("report", "coordinator",
                "Summarize position health {{steps.check-positions.output}} and risk {{steps.assess-alerts.output}}.",
                &["check-positions", "assess-alerts"]),
        ]),

        _ => return None,
    };
    Some(steps)
}

/// The agent worker tags the gateway exposes as nullboiler webhook workers.
pub const AGENT_WORKERS: &[&str] = &[
    "scout", "risk", "trader", "executor", "sentinel",
    "yield", "sniper", "coordinator", "launcher",
];

/// Register this gateway's agent workers with nullboiler so it can dispatch DAG
/// steps back to us. Best-effort and idempotent: a 409 (already registered) is
/// treated as success. No-op when `callback_url` is empty (integration off).
pub async fn register_agent_workers(
    client: &reqwest::Client,
    nullboiler_url: &str,
    callback_url: &str,
    token: &str,
) {
    if callback_url.trim().is_empty() {
        tracing::info!("nullboiler worker registration skipped (VESPRA_WORKER_CALLBACK_URL unset)");
        return;
    }
    let base = callback_url.trim_end_matches('/');
    let url = format!("{}/workers", nullboiler_url.trim_end_matches('/'));

    // Retry the batch: nullboiler may start after the gateway (no health gate
    // between them), so a first-pass failure is expected, not fatal. Only
    // still-unregistered agents are retried each round.
    const MAX_ATTEMPTS: u32 = 6;
    let mut pending: Vec<&&str> = AGENT_WORKERS.iter().collect();
    for attempt in 1..=MAX_ATTEMPTS {
        let mut still_pending = Vec::new();
        for agent in pending {
            let body = json!({
                "id": format!("vespra-{agent}"),
                "url": format!("{base}/nullboiler/worker/{agent}"),
                "token": token,
                "protocol": "webhook",
                "tags": [agent],
                "max_concurrent": 2,
            });
            let done = match client.post(&url).json(&body)
                .timeout(std::time::Duration::from_secs(10)).send().await
            {
                // success or already-registered (409) both count as done.
                Ok(resp) => resp.status().is_success() || resp.status() == reqwest::StatusCode::CONFLICT,
                Err(_) => false,
            };
            if !done {
                still_pending.push(agent);
            }
        }
        pending = still_pending;
        if pending.is_empty() {
            tracing::info!("nullboiler: registered all {} agent workers → {base}", AGENT_WORKERS.len());
            return;
        }
        if attempt < MAX_ATTEMPTS {
            tracing::warn!(
                "nullboiler: {} worker(s) not yet registered (attempt {attempt}/{MAX_ATTEMPTS}) — retrying",
                pending.len()
            );
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
    tracing::warn!(
        "nullboiler: gave up registering {} worker(s) after {MAX_ATTEMPTS} attempts — is nullboiler reachable at {url}?",
        pending.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dag_produces_valid_shape() {
        let wf = workflow_for("yield_rotation", Some("w1"), Some("base")).unwrap();
        // top-level shape nullboiler requires
        assert!(wf["steps"].is_array());
        assert!(!wf["steps"].as_array().unwrap().is_empty());
        assert_eq!(wf["input"]["wallet"], "w1");
        assert_eq!(wf["input"]["chain"], "base");
        // every step has id + type + worker_tags; deps reference real ids
        let ids: Vec<String> = wf["steps"].as_array().unwrap().iter()
            .map(|s| s["id"].as_str().unwrap().to_string()).collect();
        for s in wf["steps"].as_array().unwrap() {
            assert!(s["id"].is_string());
            assert_eq!(s["type"], "task");
            assert!(s["worker_tags"].as_array().map(|t| !t.is_empty()).unwrap_or(false));
            for d in s["depends_on"].as_array().unwrap() {
                assert!(ids.contains(&d.as_str().unwrap().to_string()), "dangling dep {d}");
            }
        }
    }

    #[test]
    fn templates_use_steps_prefix() {
        // guard against the broken `{{id.output}}` form nullboiler won't resolve
        let wf = workflow_for("yield-deposit", None, None).unwrap();
        let blob = wf.to_string();
        assert!(blob.contains("{{steps.find-yield.output}}"));
        assert!(!blob.contains("{{find-yield.output}}"));
    }

    #[test]
    fn unknown_dag_is_none() {
        assert!(workflow_for("does-not-exist", None, None).is_none());
        assert!(!is_registered("does-not-exist"));
        assert!(is_registered("trade_up"));
    }
}
