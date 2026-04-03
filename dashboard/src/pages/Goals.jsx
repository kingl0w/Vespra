import { useState, useEffect, useCallback, useRef } from "preact/hooks";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

// ── Status helpers ──────────────────────────────────────────

const STATUS_CFG = {
  Pending:    { variant: "default", label: "Pending" },
  Running:    { variant: "green",   label: "Running",   pulse: true },
  Paused:     { variant: "yellow",  label: "Paused" },
  Cancelled:  { variant: "red",     label: "Cancelled" },
  Completed:  { variant: "accent",  label: "Completed" },
  Failed:     { variant: "red",     label: "Failed" },
};

function StatusBadge({ status }) {
  const cfg = STATUS_CFG[status] || STATUS_CFG.Pending;
  return (
    <span class={cfg.pulse ? "animate-pulse" : ""}>
      <Badge variant={cfg.variant}>{cfg.label}</Badge>
    </span>
  );
}

function rowBorder(status) {
  if (status === "Running") return "border-l-2 border-l-vespra-green";
  if (status === "Failed")  return "border-l-2 border-l-vespra-red";
  return "border-l-2 border-l-transparent";
}

// ── Elapsed time ────────────────────────────────────────────

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

// ── New Goal form ───────────────────────────────────────────

function GoalForm({ wallets, onCreated }) {
  const [text, setText] = useState("");
  const [walletId, setWalletId] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState(null);

  const submit = async () => {
    if (submitting || !text.trim()) return;
    setSubmitting(true);
    setError(null);
    try {
      await api.createGoal({
        goal: text.trim(),
        ...(walletId ? { wallet_id: walletId } : {}),
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
          value={walletId}
          onChange={(e) => setWalletId(e.target.value)}
          class="bg-vespra-bg border border-vespra-border rounded px-2 py-2 text-sm text-vespra-text cursor-pointer"
          aria-label="Select wallet"
        >
          <option value="">No wallet (auto)</option>
          {(wallets || []).map((w) => (
            <option key={w.id} value={w.id}>
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

// ── Goal row ────────────────────────────────────────────────

function GoalRow({ goal, onAction }) {
  const pnl = goal.pnl_eth ?? 0;
  const pnlPct = goal.pnl_pct ?? 0;
  const positive = pnl >= 0;
  const acting = useRef(false);

  const act = async (action) => {
    if (acting.current) return;
    acting.current = true;
    try {
      if (action === "cancel") await api.cancelGoal(goal.id);
      else if (action === "pause") await api.pauseGoal(goal.id);
      else if (action === "resume") await api.resumeGoal(goal.id);
      onAction();
    } catch { /* swallow — refresh will show current state */ }
    acting.current = false;
  };

  const canPause  = goal.status === "Running";
  const canResume = goal.status === "Paused";
  const canCancel = goal.status === "Running" || goal.status === "Paused";

  return (
    <tr class={`${rowBorder(goal.status)} hover:bg-vespra-border/30 transition-colors`}>
      <td class="px-3 py-2 text-sm max-w-[240px]">
        <span title={goal.goal}>
          {goal.goal?.length > 60 ? goal.goal.slice(0, 60) + "..." : goal.goal}
        </span>
      </td>
      <td class="px-3 py-2"><StatusBadge status={goal.status} /></td>
      <td class="px-3 py-2 text-xs text-vespra-muted font-mono">{goal.current_step || "--"}</td>
      <td class="px-3 py-2 text-sm font-mono">{goal.entry_eth != null ? `${goal.entry_eth} ETH` : "--"}</td>
      <td class="px-3 py-2 text-sm font-mono">{goal.current_eth != null ? `${goal.current_eth} ETH` : "--"}</td>
      <td class={`px-3 py-2 text-sm font-mono ${positive ? "text-vespra-green" : "text-vespra-red"}`}>
        {pnl !== 0 ? `${positive ? "+" : ""}${pnl.toFixed(4)} ETH (${positive ? "+" : ""}${pnlPct.toFixed(1)}%)` : "--"}
      </td>
      <td class="px-3 py-2 text-sm text-center">{goal.cycles ?? "--"}</td>
      <td class="px-3 py-2 text-xs text-vespra-muted">{elapsed(goal.created_at)}</td>
      <td class="px-3 py-2">
        <div class="flex gap-1">
          {canPause  && <button onClick={() => act("pause")}  class="px-2 py-1 text-xs rounded bg-vespra-yellow/15 text-vespra-yellow hover:bg-vespra-yellow/25 transition-colors">Pause</button>}
          {canResume && <button onClick={() => act("resume")} class="px-2 py-1 text-xs rounded bg-vespra-green/15 text-vespra-green hover:bg-vespra-green/25 transition-colors">Resume</button>}
          {canCancel && <button onClick={() => act("cancel")} class="px-2 py-1 text-xs rounded bg-vespra-red/15 text-vespra-red hover:bg-vespra-red/25 transition-colors">Cancel</button>}
        </div>
      </td>
    </tr>
  );
}

// ── Main page ───────────────────────────────────────────────

export function Goals() {
  const [goals, setGoals] = useState(null);
  const [wallets, setWallets] = useState([]);
  const [loading, setLoading] = useState(true);
  const intervalRef = useRef(null);

  const fetchGoals = useCallback(() => {
    api.fetchGoals()
      .then((data) => setGoals(Array.isArray(data) ? data : data?.goals || []))
      .catch(() => setGoals([]))
      .finally(() => setLoading(false));
  }, []);

  // Initial load
  useEffect(() => {
    fetchGoals();
    api.walletList()
      .then((data) => setWallets(Array.isArray(data) ? data : data?.wallets || []))
      .catch(() => {});
  }, [fetchGoals]);

  // Auto-refresh when any goal is Running or Paused
  useEffect(() => {
    const needsPoll = goals?.some((g) => g.status === "Running" || g.status === "Paused");
    if (needsPoll) {
      intervalRef.current = setInterval(fetchGoals, 10000);
    }
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, [goals, fetchGoals]);

  if (loading && !goals) return <Loader />;

  const list = goals || [];

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Goals</h2>

      <Card title="New Goal">
        <GoalForm wallets={wallets} onCreated={fetchGoals} />
      </Card>

      <Card
        title={`Goal List (${list.length})`}
        actions={<Button variant="ghost" onClick={fetchGoals}>Refresh</Button>}
      >
        {list.length === 0 ? (
          <p class="text-sm text-vespra-muted py-6 text-center">
            No goals yet. Describe a goal above to start.
          </p>
        ) : (
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
                {list.map((g) => (
                  <GoalRow key={g.id} goal={g} onAction={fetchGoals} />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
