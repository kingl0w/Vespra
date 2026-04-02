import { useState, useEffect } from "preact/hooks";
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

function Toast({ toast, onDone }) {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (!toast) return;
    // Trigger fade-in on next frame
    const frameId = requestAnimationFrame(() => setVisible(true));
    const timer = setTimeout(() => setVisible(false), 3000);
    const cleanup = setTimeout(onDone, 3300);
    return () => {
      cancelAnimationFrame(frameId);
      clearTimeout(timer);
      clearTimeout(cleanup);
    };
  }, [toast]);

  const runId = toast?.runId || "";
  const truncated = runId.length > 8 ? runId.slice(0, 8) + "..." : runId;

  return (
    <div class="fixed bottom-4 right-4 z-50" aria-live="polite" aria-atomic="true">
      {toast && (
        <div
          class="transition-opacity duration-300 cursor-pointer"
          style={{ opacity: visible ? 1 : 0 }}
          onClick={onDone}
          role="status"
        >
          <div class="bg-vespra-surface border border-vespra-green/30 rounded-lg px-4 py-3 shadow-lg flex items-center gap-3">
            <span class="text-sm text-vespra-green font-medium">Pipeline submitted</span>
            {truncated && (
              <span class="font-mono text-xs bg-vespra-surface px-2 py-0.5 rounded border border-vespra-border text-vespra-muted">{truncated}</span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

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
  const [error, setError] = useState(null);
  const [toast, setToast] = useState(null);
  const { data: runs, loading, refresh } = usePolling(() => api.dagList().catch(() => []), 8000);

  const submit = async (key) => {
    if (submitting) return; // prevent concurrent submissions
    setSubmitting(key);
    setError(null);
    try {
      const res = await api.dagSubmit(PRESETS[key].dag);
      setToast({ runId: res.run_id || res.id || "" });
      refresh();
    } catch (err) {
      setError(err.error || err.message || "Pipeline submission failed");
    } finally {
      setSubmitting(null);
    }
  };

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Pipeline Control</h2>

      <Card title="Launch Pipeline">
        <div class="grid grid-cols-1 sm:grid-cols-2 md:grid-cols-4 gap-3">
          {Object.entries(PRESETS).map(([key, preset]) => (
            <Button
              key={key}
              variant="accent"
              onClick={() => submit(key)}
              disabled={!!submitting}
              className="py-3"
            >
              {submitting === key ? "Submitting..." : preset.name}
            </Button>
          ))}
        </div>
        {error && (
          <div class="mt-3 text-sm text-vespra-red flex items-center gap-2" role="alert">
            <span>{error}</span>
            <button
              onClick={() => setError(null)}
              class="text-vespra-muted hover:text-vespra-text text-xs underline"
            >
              Dismiss
            </button>
          </div>
        )}
      </Card>

      <Toast toast={toast} onDone={() => setToast(null)} />

      <Card title="DAG Runs" actions={<Button variant="ghost" onClick={refresh}>Refresh</Button>}>
        {(() => {
          const items = Array.isArray(runs) ? runs : runs?.items || [];
          if (loading && !runs) return <Loader />;
          if (items.length === 0) return <p class="text-vespra-muted text-sm">No DAG runs found</p>;
          return (
            <div class="overflow-x-auto">
              <table class="w-full text-sm">
                <thead>
                  <tr class="text-left text-xs text-vespra-muted border-b border-vespra-border">
                    <th scope="col" class="py-2.5 px-3 font-medium">ID</th>
                    <th scope="col" class="py-2.5 px-3 font-medium">Status</th>
                    <th scope="col" class="py-2.5 px-3 font-medium">Created</th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((run) => {
                    const st = run.status || "unknown";
                    const variant = st === "running" ? "yellow" : st === "completed" ? "green" : st === "failed" ? "red" : "default";
                    return (
                      <tr key={run.id} class="border-b border-vespra-border hover:bg-vespra-border/30 transition-colors">
                        <td class="py-2.5 px-3 font-mono text-xs">{(run.id || "").slice(0, 8)}</td>
                        <td class="py-2.5 px-3"><Badge variant={variant}>{st}</Badge></td>
                        <td class="py-2.5 px-3 text-vespra-muted text-xs">
                          {run.created_at_ms ? new Date(run.created_at_ms).toLocaleString() : "-"}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          );
        })()}
      </Card>
    </div>
  );
}
