import { Component } from "preact";
import { useState, useEffect, useCallback, useRef } from "preact/hooks";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";


const STATUS_CFG = {
  pending:    { variant: "default", label: "Pending" },
  running:    { variant: "green",   label: "Running",   pulse: true },
  paused:     { variant: "yellow",  label: "Paused" },
  cancelled:  { variant: "red",     label: "Cancelled" },
  completed:  { variant: "accent",  label: "Completed" },
  failed:     { variant: "red",     label: "Failed" },
};

const PAGE_SIZE = 25;

function StatusBadge({ status }) {
  const key = (status || "").toLowerCase();
  const cfg = STATUS_CFG[key] || STATUS_CFG.pending;
  return (
    <span class={cfg.pulse ? "animate-pulse" : ""}>
      <Badge variant={cfg.variant}>{cfg.label}</Badge>
    </span>
  );
}

function rowBorder(status) {
  const key = (status || "").toLowerCase();
  if (key === "running") return "border-l-2 border-l-vespra-green";
  if (key === "failed")  return "border-l-2 border-l-vespra-red";
  return "border-l-2 border-l-transparent";
}

//── elapsed time ────────────────────────────────────────────

function elapsed(iso) {
  if (!iso) return "--";
  const ms = Date.now() - new Date(iso).getTime();
  const s = Math.floor(ms / 1000);
  if (s < 60)   return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return `${h}h ${m}m`;
}

function truncate(s, n) {
  if (!s) return "";
  return s.length > n ? s.slice(0, n) + "…" : s;
}

//── new goal form ───────────────────────────────────────────

function GoalForm({ wallets, onCreated }) {
  const [text, setText] = useState("");
  const [walletLabel, setWalletLabel] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState(null);

  const submit = async () => {
    if (submitting || !text.trim()) return;
    setSubmitting(true);
    setError(null);
    try {
      const result = await api.createGoal({
        raw_goal: text.trim(),
        ...(walletLabel ? { wallet_label: walletLabel } : {}),
      });
      console.log("[goal created]", {
        goal_id: result?.id,
        wallet_label: walletLabel || "(auto)",
      });
      setText("");
      onCreated();
    } catch (e) {
      setError(e.error || e.message || "Failed to create goal");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div class="space-y-3">
      <textarea
        value={text}
        onInput={(e) => setText(e.target.value)}
        placeholder="Describe your goal in plain English..."
        rows={3}
        class="w-full bg-vespra-bg border border-vespra-border rounded px-3 py-2 text-sm text-vespra-text placeholder:text-vespra-muted/50 resize-none focus:outline-none focus:border-vespra-accent/50"
      />
      <div class="flex items-center gap-3">
        <select
          value={walletLabel}
          onChange={(e) => setWalletLabel(e.target.value)}
          class="bg-vespra-bg border border-vespra-border rounded px-2 py-2 text-sm text-vespra-text cursor-pointer"
          aria-label="Select wallet"
        >
          <option value="">No wallet (auto)</option>
          {(wallets || []).map((w) => (
            <option key={w.id} value={w.label || ""}>
              {w.label || w.id.slice(0, 8)}
            </option>
          ))}
        </select>
        <Button variant="accent" onClick={submit} disabled={submitting || !text.trim()}>
          {submitting ? "Starting..." : "Start Goal"}
        </Button>
      </div>
      {error && <p class="text-xs text-vespra-red">{error}</p>}
    </div>
  );
}

//── goal row ────────────────────────────────────────────────

function GoalRow({ goal, onAction, onOpen }) {
  const pnl = goal.pnl_eth ?? 0;
  const pnlPct = goal.pnl_pct ?? 0;
  const positive = pnl >= 0;
  const acting = useRef(false);
  const [actionError, setActionError] = useState(null);

  const act = async (e, action) => {
    //ves-fix: stop row click from opening the detail modal.
    e.stopPropagation();
    if (acting.current) return;
    acting.current = true;
    setActionError(null);
    try {
      if (action === "cancel") await api.cancelGoal(goal.id);
      else if (action === "pause") await api.pauseGoal(goal.id);
      else if (action === "resume") await api.resumeGoal(goal.id);
      onAction();
    } catch (e) {
      console.error(`[Goals] ${action} failed for goal ${goal.id}:`, e);
      setActionError("Action failed — please try again");
    }
    acting.current = false;
  };

  const statusKey = (goal.status || "").toLowerCase();
  const canPause  = statusKey === "running";
  const canResume = statusKey === "paused";
  const canCancel = statusKey === "running" || statusKey === "paused";

  //api returns the natural-language goal under `raw_goal`.
  const goalText = goal.raw_goal || "";

  //ves-fix: render error or completion reason as a secondary line under the
  //status badge. the backend stores both in goal.error (failures and final
  //completion messages alike), so branch on status to pick the colour.
  const detail = goal.error ? truncate(goal.error, 120) : null;
  const detailClass =
    statusKey === "failed"
      ? "text-vespra-red"
      : statusKey === "completed"
      ? "text-vespra-muted"
      : null;

  return (
    <tr
      onClick={() => onOpen(goal)}
      class={`${rowBorder(goal.status)} hover:bg-vespra-border/30 transition-colors cursor-pointer`}
    >
      <td class="px-3 py-2 text-sm max-w-[240px]">
        <div class="flex flex-col">
          <span title={goalText}>
            {goalText.length > 40 ? goalText.slice(0, 40) + "…" : goalText}
          </span>
          {/* VES-120: surface wallet_label as a subtle secondary line so the
              owner of each goal is visible without opening detail. */}
          {goal.wallet_label && (
            <span class="text-[10px] text-vespra-muted font-mono mt-0.5">
              {goal.wallet_label}
            </span>
          )}
        </div>
      </td>
      <td class="px-3 py-2">
        <div class="flex flex-col gap-0.5">
          <StatusBadge status={goal.status} />
          {detail && detailClass && (
            <span class={`text-[10px] ${detailClass} max-w-[220px]`} title={goal.error}>
              {detail}
            </span>
          )}
        </div>
      </td>
      <td class="px-3 py-2 text-xs text-vespra-muted font-mono">{(statusKey === "failed" ? goal.failed_at_step : null) || goal.current_step || "--"}</td>
      <td class="px-3 py-2 text-sm font-mono">{goal.entry_eth != null ? `${goal.entry_eth} ETH` : "--"}</td>
      <td class="px-3 py-2 text-sm font-mono">{goal.current_eth != null ? `${goal.current_eth} ETH` : "--"}</td>
      <td class={`px-3 py-2 text-sm font-mono ${positive ? "text-vespra-green" : "text-vespra-red"}`}>
        {pnl !== 0 ? `${positive ? "+" : ""}${pnl.toFixed(4)} ETH (${positive ? "+" : ""}${pnlPct.toFixed(1)}%)` : "--"}
      </td>
      <td class="px-3 py-2 text-sm text-center">{goal.cycles ?? "--"}</td>
      <td class="px-3 py-2 text-xs text-vespra-muted">{elapsed(goal.created_at)}</td>
      <td class="px-3 py-2">
        <div class="flex gap-1">
          {canPause  && <button onClick={(e) => act(e, "pause")}  class="px-2 py-1 text-xs rounded bg-vespra-yellow/15 text-vespra-yellow hover:bg-vespra-yellow/25 transition-colors">Pause</button>}
          {canResume && <button onClick={(e) => act(e, "resume")} class="px-2 py-1 text-xs rounded bg-vespra-green/15 text-vespra-green hover:bg-vespra-green/25 transition-colors">Resume</button>}
          {canCancel && <button onClick={(e) => act(e, "cancel")} class="px-2 py-1 text-xs rounded bg-vespra-red/15 text-vespra-red hover:bg-vespra-red/25 transition-colors">Cancel</button>}
        </div>
        {actionError && <p class="mt-1 text-xs text-vespra-red">{actionError}</p>}
      </td>
    </tr>
  );
}

//── goal detail modal ──────────────────────────────────────

function DetailRow({ label, value, mono = false }) {
  if (value == null || value === "") return null;
  return (
    <div class="flex items-start gap-3 py-1.5 border-b border-vespra-border/40 last:border-b-0">
      <span class="text-xs text-vespra-muted w-32 shrink-0 uppercase tracking-wider">{label}</span>
      <span class={`text-sm text-vespra-text break-all ${mono ? "font-mono" : ""}`}>{value}</span>
    </div>
  );
}

function GoalDetailModal({ goalId, onClose }) {
  const [goal, setGoal] = useState(null);
  const [err, setErr] = useState(null);

  useEffect(() => {
    let cancelled = false;
    api.fetchGoal(goalId)
      .then((g) => { if (!cancelled) setGoal(g); })
      .catch((e) => { if (!cancelled) setErr(e?.error || e?.message || "Failed to load goal"); });
    return () => { cancelled = true; };
  }, [goalId]);

  //ves-fix: close on Escape so keyboard users aren't trapped in the modal.
  useEffect(() => {
    const onKey = (e) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const backdrop = (e) => { if (e.target === e.currentTarget) onClose(); };

  return (
    <div
      onClick={backdrop}
      class="fixed inset-0 z-50 bg-black/70 backdrop-blur-sm flex items-center justify-center p-4"
      role="dialog"
      aria-modal="true"
    >
      <div class="bg-vespra-surface border border-vespra-border rounded-lg w-full max-w-2xl max-h-[85vh] overflow-hidden flex flex-col shadow-xl">
        <div class="flex items-center justify-between px-5 py-3 border-b border-vespra-border">
          <h3 class="text-sm font-semibold text-vespra-text">Goal Detail</h3>
          <button
            onClick={onClose}
            class="text-vespra-muted hover:text-vespra-accent text-xl leading-none px-2"
            aria-label="Close"
          >
            ×
          </button>
        </div>
        <div class="p-5 overflow-y-auto">
          {err && <p class="text-sm text-vespra-red">{err}</p>}
          {!goal && !err && <Loader />}
          {goal && (
            <div class="space-y-4">
              <div class="flex items-center gap-2">
                <StatusBadge status={goal.status} />
                <span class="text-xs text-vespra-muted font-mono">{goal.id}</span>
              </div>

              <section>
                <h4 class="text-[11px] uppercase tracking-wider text-vespra-accent mb-1">Goal</h4>
                <DetailRow label="Raw Goal" value={goal.raw_goal} />
                <DetailRow label="Strategy" value={goal.strategy} />
                <DetailRow label="Chain" value={goal.chain} />
                <DetailRow label="Wallet" value={goal.wallet_label} mono />
                <DetailRow label="Capital" value={goal.capital_eth != null ? `${goal.capital_eth} ETH` : null} mono />
              </section>

              <section>
                <h4 class="text-[11px] uppercase tracking-wider text-vespra-accent mb-1">Progress</h4>
                <DetailRow
                  label={(goal.status || "").toLowerCase() === "failed" ? "Failed At Step" : "Current Step"}
                  value={(goal.status || "").toLowerCase() === "failed" ? (goal.failed_at_step || goal.current_step) : goal.current_step}
                  mono
                />
                <DetailRow label="Cycles" value={goal.cycles} mono />
                <DetailRow label="Entry" value={goal.entry_eth != null ? `${goal.entry_eth} ETH` : null} mono />
                <DetailRow label="Current" value={goal.current_eth != null ? `${goal.current_eth} ETH` : null} mono />
                <DetailRow
                  label="P&L"
                  value={
                    goal.pnl_eth != null
                      ? `${goal.pnl_eth >= 0 ? "+" : ""}${Number(goal.pnl_eth).toFixed(4)} ETH (${goal.pnl_pct >= 0 ? "+" : ""}${Number(goal.pnl_pct || 0).toFixed(2)}%)`
                      : null
                  }
                  mono
                />
              </section>

              {(goal.token_address || goal.token_amount_held) && (
                <section>
                  <h4 class="text-[11px] uppercase tracking-wider text-vespra-accent mb-1">Position</h4>
                  <DetailRow label="Token Address" value={goal.token_address} mono />
                  <DetailRow label="Token Amount" value={goal.token_amount_held} mono />
                </section>
              )}

              {goal.error && (
                <section>
                  <h4 class="text-[11px] uppercase tracking-wider text-vespra-accent mb-1">
                    {(goal.status || "").toLowerCase() === "failed" ? "Error" : "Final Message"}
                  </h4>
                  <pre
                    class={`whitespace-pre-wrap break-words text-xs p-3 rounded border ${
                      (goal.status || "").toLowerCase() === "failed"
                        ? "border-vespra-red/30 bg-vespra-red/5 text-vespra-red"
                        : "border-vespra-border bg-vespra-bg/40 text-vespra-muted"
                    }`}
                  >
                    {goal.error}
                  </pre>
                </section>
              )}

              <section>
                <h4 class="text-[11px] uppercase tracking-wider text-vespra-accent mb-1">Timestamps</h4>
                <DetailRow label="Created" value={goal.created_at} mono />
                <DetailRow label="Updated" value={goal.updated_at} mono />
              </section>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

//── error boundary ─────────────────────────────────────────

class GoalListErrorBoundary extends Component {
  constructor(props) {
    super(props);
    this.state = { hasError: false };
  }
  static getDerivedStateFromError() {
    return { hasError: true };
  }
  componentDidCatch(error, info) {
    //ves-fix: keep the goals page alive if a single row throws during render;
    //log so the failure is still visible to anyone debugging in devtools.
    console.error("[Goals] error boundary caught render error:", error, info);
  }
  render() {
    if (this.state.hasError) {
      return (
        <div class="py-8 text-center space-y-3">
          <p class="text-sm text-vespra-red">One or more goals failed to render.</p>
          <Button variant="default" onClick={() => window.location.reload()}>
            Refresh
          </Button>
        </div>
      );
    }
    return this.props.children;
  }
}

//── main page ───────────────────────────────────────────────

export function Goals() {
  const [goals, setGoals] = useState(null);
  const [wallets, setWallets] = useState([]);
  const [loading, setLoading] = useState(true);
  const [page, setPage] = useState(0);
  const [selectedId, setSelectedId] = useState(null);
  const intervalRef = useRef(null);

  const fetchGoals = useCallback(() => {
    api.fetchGoals()
      .then((data) => setGoals(Array.isArray(data) ? data : data?.goals || []))
      .catch(() => setGoals([]))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    fetchGoals();
    api.walletList()
      .then((data) => setWallets(Array.isArray(data) ? data : data?.wallets || []))
      .catch(() => {});
  }, [fetchGoals]);

  useEffect(() => {
    const needsPoll = goals?.some((g) => {
      const k = (g.status || "").toLowerCase();
      return k === "running" || k === "paused";
    });
    if (needsPoll) {
      intervalRef.current = setInterval(fetchGoals, 10000);
    }
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [goals, fetchGoals]);

  if (loading && !goals) return <Loader />;

  //ves-fix: sort most-recent-first by created_at so new goals always lead
  //page 1. falls back to insertion order for goals missing created_at.
  const sorted = [...(goals || [])].sort((a, b) => {
    const at = a.created_at ? new Date(a.created_at).getTime() : 0;
    const bt = b.created_at ? new Date(b.created_at).getTime() : 0;
    return bt - at;
  });

  const total = sorted.length;
  const pageCount = Math.max(1, Math.ceil(total / PAGE_SIZE));
  const clampedPage = Math.min(page, pageCount - 1);
  const start = clampedPage * PAGE_SIZE;
  const end = Math.min(start + PAGE_SIZE, total);
  const pageRows = sorted.slice(start, end);

  const groups = pageRows.reduce((acc, g) => {
    const chain = (g.chain || "unknown").toLowerCase();
    (acc[chain] ||= []).push(g);
    return acc;
  }, {});

  const CHAIN_ORDER = ["base", "arbitrum"];
  const orderedChains = Object.keys(groups).sort((a, b) => {
    if (a === "unknown") return 1;
    if (b === "unknown") return -1;
    const ai = CHAIN_ORDER.indexOf(a);
    const bi = CHAIN_ORDER.indexOf(b);
    if (ai !== -1 && bi !== -1) return ai - bi;
    if (ai !== -1) return -1;
    if (bi !== -1) return 1;
    return a.localeCompare(b);
  });

  const chainTitle = (c) =>
    c === "unknown" ? "Unknown" : c.charAt(0).toUpperCase() + c.slice(1);

  const activeCount = (rows) =>
    rows.filter((g) => {
      const k = (g.status || "").toLowerCase();
      return k === "running" || k === "monitoring";
    }).length;

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Goals</h2>

      <Card title="New Goal">
        <GoalForm wallets={wallets} onCreated={fetchGoals} />
      </Card>

      <Card
        title={`Goal List (${total})`}
        actions={<Button variant="ghost" onClick={fetchGoals}>Refresh</Button>}
      >
        <GoalListErrorBoundary>
          {total === 0 ? (
            <p class="text-sm text-vespra-muted py-6 text-center">
              No goals yet. Describe a goal above to start.
            </p>
          ) : (
            <>
              <div class="overflow-x-auto -mx-4">
                <table class="w-full text-left">
                  <thead>
                    <tr class="text-xs text-vespra-muted border-b border-vespra-border">
                      <th class="px-3 py-2 font-medium">Goal</th>
                      <th class="px-3 py-2 font-medium">Status</th>
                      <th class="px-3 py-2 font-medium">Step</th>
                      <th class="px-3 py-2 font-medium">Capital</th>
                      <th class="px-3 py-2 font-medium">Value</th>
                      <th class="px-3 py-2 font-medium">P&L</th>
                      <th class="px-3 py-2 font-medium text-center">Cycles</th>
                      <th class="px-3 py-2 font-medium">Elapsed</th>
                      <th class="px-3 py-2 font-medium">Actions</th>
                    </tr>
                  </thead>
                  <tbody class="divide-y divide-vespra-border/50">
                    {orderedChains.map((chain) => {
                      const rows = groups[chain];
                      return (
                        <>
                          <tr key={`hdr-${chain}`} class="bg-vespra-bg/40">
                            <td
                              colSpan={9}
                              class="px-3 py-2 text-[11px] uppercase tracking-wider text-vespra-muted border-t border-vespra-border/60"
                            >
                              {chainTitle(chain)} — {activeCount(rows)} active
                            </td>
                          </tr>
                          {rows.map((g) => (
                            <GoalRow
                              key={g.id}
                              goal={g}
                              onAction={fetchGoals}
                              onOpen={(goal) => setSelectedId(goal.id)}
                            />
                          ))}
                        </>
                      );
                    })}
                  </tbody>
                </table>
              </div>
              <div class="flex items-center justify-between pt-4 mt-2 border-t border-vespra-border">
                <span class="text-xs text-vespra-muted">
                  Showing {total === 0 ? 0 : start + 1}-{end} of {total} goals
                </span>
                <div class="flex gap-2">
                  <Button
                    variant="default"
                    onClick={() => setPage((p) => Math.max(0, p - 1))}
                    disabled={clampedPage === 0}
                  >
                    Previous
                  </Button>
                  <Button
                    variant="default"
                    onClick={() => setPage((p) => Math.min(pageCount - 1, p + 1))}
                    disabled={clampedPage >= pageCount - 1}
                  >
                    Next
                  </Button>
                </div>
              </div>
            </>
          )}
        </GoalListErrorBoundary>
      </Card>

      {selectedId && (
        <GoalDetailModal goalId={selectedId} onClose={() => setSelectedId(null)} />
      )}
    </div>
  );
}
