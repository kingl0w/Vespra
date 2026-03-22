import { useState } from "preact/hooks";
import { usePolling } from "../hooks/useApi.js";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

const PRESETS = {
  "swap-with-risk-check": {
    name: "Swap with Risk Check",
    dag: {
      name: "swap-with-risk-check",
      steps: [
        {
          id: "risk-assess",
          worker_tags: ["risk"],
          prompt_template: "Evaluate risk for swapping 0.01 ETH to USDC on Base via Uniswap V3",
        },
        {
          id: "build-swap",
          worker_tags: ["trader"],
          prompt_template: "Build a swap: 0.01 ETH -> USDC on Base via Uniswap V3 router",
          depends_on: ["risk-assess"],
        },
        {
          id: "execute-swap",
          worker_tags: ["executor"],
          prompt_template: "Execute this swap plan: {{build-swap.output}}",
          depends_on: ["build-swap"],
        },
      ],
    },
  },
  "yield-deposit": {
    name: "Yield Deposit",
    dag: {
      name: "yield-deposit",
      steps: [
        {
          id: "find-yield",
          worker_tags: ["scout"],
          prompt_template: "Find the best stablecoin yield opportunity on Base or Arbitrum right now",
        },
        {
          id: "assess-risk",
          worker_tags: ["risk"],
          prompt_template: "Evaluate risk for: {{find-yield.output}}",
          depends_on: ["find-yield"],
        },
        {
          id: "build-deposit",
          worker_tags: ["yield"],
          prompt_template: "Build a deposit plan for the best opportunity from: {{find-yield.output}}",
          depends_on: ["find-yield", "assess-risk"],
        },
      ],
    },
  },
  "health-monitor": {
    name: "Health Monitor",
    dag: {
      name: "health-monitor",
      steps: [
        {
          id: "check-positions",
          worker_tags: ["sentinel"],
          prompt_template: "Check all active DeFi positions for health warnings",
        },
        {
          id: "assess-alerts",
          worker_tags: ["risk"],
          prompt_template: "Evaluate these position alerts: {{check-positions.output}}",
          depends_on: ["check-positions"],
        },
        {
          id: "report",
          worker_tags: ["coordinator"],
          prompt_template: "Summarize position health and risk assessment: Positions: {{check-positions.output}} Risk: {{assess-alerts.output}}",
          depends_on: ["check-positions", "assess-alerts"],
        },
      ],
    },
  },
  "token-launch": {
    name: "Token Launch",
    dag: {
      name: "token-launch",
      steps: [
        {
          id: "design-token",
          worker_tags: ["launcher"],
          prompt_template: "Design a standard ERC-20 token with 1M supply on Base Sepolia. Name: VespraTest, Symbol: VTST. Use wallet deployer-01.",
        },
        {
          id: "risk-review",
          worker_tags: ["risk"],
          prompt_template: "Review this token deployment plan for risks: {{design-token.output}}",
          depends_on: ["design-token"],
        },
        {
          id: "report",
          worker_tags: ["coordinator"],
          prompt_template: "Summarize token launch plan and risk review: Plan: {{design-token.output}} Risk: {{risk-review.output}}",
          depends_on: ["design-token", "risk-review"],
        },
      ],
    },
  },
};

function StepBadge({ status }) {
  const variants = {
    completed: "green",
    running: "accent",
    ready: "yellow",
    pending: "default",
    failed: "red",
  };
  return <Badge variant={variants[status] || "default"}>{status}</Badge>;
}

function RunCard({ run }) {
  return (
    <Card title={run.name || run.id} className="text-sm">
      <div class="flex items-center gap-3 mb-3">
        <StepBadge status={run.status} />
        <span class="text-vespra-muted text-xs">{run.id}</span>
      </div>
      {run.steps && (
        <div class="space-y-2">
          {run.steps.map((step) => (
            <div key={step.id} class="flex items-center justify-between py-1 border-b border-vespra-border last:border-0">
              <span class="text-vespra-text">{step.id}</span>
              <StepBadge status={step.status} />
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}

export function Pipelines() {
  const [submitting, setSubmitting] = useState(null);
  const [result, setResult] = useState(null);
  const { data: runs, loading, refresh } = usePolling(() => api.dagList().catch(() => []), 8000);

  const submit = async (key) => {
    setSubmitting(key);
    setResult(null);
    try {
      const res = await api.dagSubmit(PRESETS[key].dag);
      setResult({ ok: true, msg: `Submitted: ${res.run_id || res.id || "OK"}` });
      refresh();
    } catch (err) {
      setResult({ ok: false, msg: err.error || JSON.stringify(err) });
    } finally {
      setSubmitting(null);
    }
  };

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Pipeline Control</h2>

      <Card title="Launch Pipeline">
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
          {Object.entries(PRESETS).map(([key, preset]) => (
            <Button
              key={key}
              variant="accent"
              onClick={() => submit(key)}
              disabled={submitting === key}
              className="py-3"
            >
              {submitting === key ? "Submitting..." : preset.name}
            </Button>
          ))}
        </div>
        {result && (
          <div class={`mt-3 text-sm ${result.ok ? "text-vespra-green" : "text-vespra-red"}`}>
            {result.msg}
          </div>
        )}
      </Card>

      <Card title="DAG Runs" actions={<Button variant="ghost" onClick={refresh}>Refresh</Button>}>
        {loading && !runs ? (
          <Loader />
        ) : !runs || (Array.isArray(runs) && runs.length === 0) ? (
          <p class="text-vespra-muted text-sm">No DAG runs found</p>
        ) : Array.isArray(runs) ? (
          <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
            {runs.map((run) => (
              <RunCard key={run.id} run={run} />
            ))}
          </div>
        ) : (
          <pre class="text-xs text-vespra-muted overflow-auto">{JSON.stringify(runs, null, 2)}</pre>
        )}
      </Card>
    </div>
  );
}
