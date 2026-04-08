import { useState, useEffect, useCallback, useRef } from "preact/hooks";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

// ── Date helpers ─────────────────────────────────────────────

function todayIso() {
  return new Date().toISOString().slice(0, 10);
}

function daysAgoIso(n) {
  const d = new Date();
  d.setUTCDate(d.getUTCDate() - n);
  return d.toISOString().slice(0, 10);
}

function fmtPct(v) {
  if (v == null || Number.isNaN(v)) return "--";
  const n = Number(v);
  return `${n >= 0 ? "+" : ""}${n.toFixed(2)}%`;
}

function fmtEth(v) {
  if (v == null || Number.isNaN(v)) return "--";
  return `${Number(v).toFixed(4)} ETH`;
}

function fmtMode(mode) {
  return mode === "agents" ? "Agent-based" : "Rule-based";
}

// ── Mode selector with tooltip ──────────────────────────────

function ModeSelector({ mode, onChange }) {
  const [tipOpen, setTipOpen] = useState(false);
  const opts = [
    { id: "rules", label: "Rule-based" },
    { id: "agents", label: "Agent-based" },
  ];
  return (
    <div class="flex items-center gap-2">
      <div class="inline-flex rounded border border-vespra-border overflow-hidden">
        {opts.map((o) => (
          <button
            key={o.id}
            type="button"
            onClick={() => onChange(o.id)}
            class={`px-3 py-2 text-sm transition-colors ${
              mode === o.id
                ? "bg-vespra-accent/15 text-vespra-accent font-medium"
                : "bg-vespra-bg text-vespra-muted hover:text-vespra-text"
            }`}
          >
            {o.label}
          </button>
        ))}
      </div>
      <div class="relative">
        <button
          type="button"
          onMouseEnter={() => setTipOpen(true)}
          onMouseLeave={() => setTipOpen(false)}
          onFocus={() => setTipOpen(true)}
          onBlur={() => setTipOpen(false)}
          class="flex items-center justify-center w-6 h-6 rounded-full border border-vespra-border text-vespra-muted text-xs hover:text-vespra-accent hover:border-vespra-accent transition-colors"
          aria-label="Mode info"
        >
          ⓘ
        </button>
        {tipOpen && (
          <div class="absolute left-full ml-2 top-1/2 -translate-y-1/2 z-20 w-72 bg-vespra-surface border border-vespra-border rounded-lg p-3 text-xs text-vespra-muted shadow-lg leading-relaxed">
            <p class="mb-2">
              <span class="text-vespra-text font-semibold">Rule-based:</span>{" "}
              Fast simulation using fixed thresholds. Free to run, good for quick iteration.
            </p>
            <p>
              <span class="text-vespra-text font-semibold">Agent-based:</span>{" "}
              Runs the full AI agent stack against historical data. More realistic but slower and consumes API credits.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Run-backtest form ───────────────────────────────────────

function BacktestForm({ wallets, onCompleted }) {
  const [text, setText] = useState("");
  const [walletLabel, setWalletLabel] = useState("");
  const [chain, setChain] = useState("base");
  const [fromDate, setFromDate] = useState(daysAgoIso(30));
  const [toDate, setToDate] = useState(todayIso());
  const [mode, setMode] = useState("rules");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState(null);

  const submit = async () => {
    if (submitting || !text.trim()) return;
    setSubmitting(true);
    setError(null);
    try {
      const result = await api.runBacktest({
        raw_goal: text.trim(),
        wallet_label: walletLabel,
        chain,
        from_date: fromDate,
        to_date: toDate,
        mode,
      });
      onCompleted(result);
    } catch (e) {
      setError(e.error || e.message || "Backtest failed");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div class="space-y-4">
      <textarea
        value={text}
        onInput={(e) => setText(e.target.value)}
        placeholder="Describe the strategy to backtest..."
        rows={3}
        class="w-full bg-vespra-bg border border-vespra-border rounded px-3 py-2 text-sm text-vespra-text placeholder:text-vespra-muted/50 resize-none focus:outline-none focus:border-vespra-accent/50"
      />

      <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3">
        <div class="flex flex-col gap-1">
          <label class="text-xs text-vespra-muted">Wallet</label>
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
        </div>

        <div class="flex flex-col gap-1">
          <label class="text-xs text-vespra-muted">Chain</label>
          <select
            value={chain}
            onChange={(e) => setChain(e.target.value)}
            class="bg-vespra-bg border border-vespra-border rounded px-2 py-2 text-sm text-vespra-text cursor-pointer"
            aria-label="Select chain"
          >
            <option value="base">base</option>
            <option value="arbitrum">arbitrum</option>
          </select>
        </div>

        <div class="flex flex-col gap-1">
          <label class="text-xs text-vespra-muted">From</label>
          <input
            type="date"
            value={fromDate}
            max={toDate}
            onInput={(e) => setFromDate(e.target.value)}
            class="bg-vespra-bg border border-vespra-border rounded px-2 py-2 text-sm text-vespra-text"
          />
        </div>

        <div class="flex flex-col gap-1">
          <label class="text-xs text-vespra-muted">To</label>
          <input
            type="date"
            value={toDate}
            min={fromDate}
            max={todayIso()}
            onInput={(e) => setToDate(e.target.value)}
            class="bg-vespra-bg border border-vespra-border rounded px-2 py-2 text-sm text-vespra-text"
          />
        </div>
      </div>

      <div class="flex flex-wrap items-center gap-4">
        <div class="flex flex-col gap-1">
          <label class="text-xs text-vespra-muted">Mode</label>
          <ModeSelector mode={mode} onChange={setMode} />
        </div>
        <div class="ml-auto">
          <Button
            variant="accent"
            onClick={submit}
            disabled={submitting || !text.trim()}
          >
            {submitting ? "Running backtest…" : "Run Backtest"}
          </Button>
        </div>
      </div>

      {error && <p class="text-xs text-vespra-red">{error}</p>}
    </div>
  );
}

// ── Equity curve chart (SVG polyline) ───────────────────────

function EquityChart({ points }) {
  if (!points || points.length === 0) {
    return (
      <p class="text-sm text-vespra-muted py-6 text-center">
        No equity-curve data available.
      </p>
    );
  }

  const W = 640;
  const H = 200;
  const PAD = 28;
  const values = points.map((p) => Number(p.value_eth) || 0);
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;

  const xStep = (W - PAD * 2) / Math.max(points.length - 1, 1);
  const coords = points.map((p, i) => {
    const x = PAD + i * xStep;
    const y = H - PAD - ((Number(p.value_eth) - min) / span) * (H - PAD * 2);
    return [x, y];
  });
  const polyline = coords.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  const areaPath = `M ${coords[0][0].toFixed(1)},${(H - PAD).toFixed(1)} L ${polyline.split(" ").join(" L ")} L ${coords[coords.length - 1][0].toFixed(1)},${(H - PAD).toFixed(1)} Z`;

  const firstDate = points[0].date;
  const lastDate = points[points.length - 1].date;

  return (
    <div class="w-full overflow-x-auto">
      <svg
        viewBox={`0 0 ${W} ${H}`}
        class="w-full h-auto"
        role="img"
        aria-label="Equity curve"
      >
        {/* baseline grid */}
        <line
          x1={PAD}
          y1={H - PAD}
          x2={W - PAD}
          y2={H - PAD}
          stroke="currentColor"
          class="text-vespra-border"
          stroke-width="1"
        />
        <line
          x1={PAD}
          y1={PAD}
          x2={PAD}
          y2={H - PAD}
          stroke="currentColor"
          class="text-vespra-border"
          stroke-width="1"
        />

        {/* filled area */}
        <path d={areaPath} class="fill-vespra-accent/10" />

        {/* line */}
        <polyline
          points={polyline}
          fill="none"
          stroke="currentColor"
          class="text-vespra-accent"
          stroke-width="2"
          stroke-linejoin="round"
          stroke-linecap="round"
        />

        {/* axis labels */}
        <text x={PAD} y={H - 6} class="fill-vespra-muted" font-size="10">
          {firstDate}
        </text>
        <text x={W - PAD} y={H - 6} text-anchor="end" class="fill-vespra-muted" font-size="10">
          {lastDate}
        </text>
        <text x={W - PAD} y={PAD - 6} text-anchor="end" class="fill-vespra-muted" font-size="10">
          max {max.toFixed(4)}
        </text>
        <text x={W - PAD} y={H - PAD - 4} text-anchor="end" class="fill-vespra-muted" font-size="10">
          min {min.toFixed(4)}
        </text>
      </svg>
    </div>
  );
}

// ── Result detail card ──────────────────────────────────────

function ResultDetail({ result }) {
  const stats = [
    { label: "P&L %",        value: fmtPct(result.pnl_pct), accent: result.pnl_pct >= 0 ? "green" : "red" },
    { label: "P&L ETH",      value: fmtEth(result.pnl_eth), accent: result.pnl_eth >= 0 ? "green" : "red" },
    { label: "Max Drawdown", value: fmtPct(-Math.abs(result.max_drawdown_pct ?? 0)), accent: "red" },
    { label: "Win Rate",     value: fmtPct(result.win_rate_pct), accent: "default" },
    { label: "Total Trades", value: result.total_trades ?? 0, accent: "default" },
    { label: "Fee Estimate", value: fmtEth(result.fee_estimate_eth), accent: "default" },
  ];

  return (
    <div class="space-y-4">
      <div class="grid grid-cols-2 sm:grid-cols-3 gap-3">
        {stats.map((s) => (
          <div
            key={s.label}
            class="bg-vespra-bg border border-vespra-border rounded p-3"
          >
            <div class="text-[11px] uppercase tracking-wider text-vespra-muted">
              {s.label}
            </div>
            <div
              class={`text-lg font-mono mt-1 ${
                s.accent === "green"
                  ? "text-vespra-green"
                  : s.accent === "red"
                  ? "text-vespra-red"
                  : "text-vespra-text"
              }`}
            >
              {s.value}
            </div>
          </div>
        ))}
      </div>
      <div>
        <div class="text-xs text-vespra-muted mb-2">Equity Curve</div>
        <EquityChart points={result.equity_curve || []} />
      </div>
    </div>
  );
}

// ── History table ───────────────────────────────────────────

function HistoryRow({ item, expanded, highlight, onToggle }) {
  return (
    <tr
      data-bt-id={item.id}
      class={`cursor-pointer hover:bg-vespra-border/30 transition-colors ${
        expanded ? "bg-vespra-border/20" : ""
      } ${highlight ? "ring-1 ring-vespra-accent/40" : ""}`}
      onClick={onToggle}
    >
      <td class="px-3 py-2 text-xs text-vespra-muted font-mono">
        {item.created_at ? item.created_at.slice(0, 10) : "--"}
      </td>
      <td class="px-3 py-2 text-sm max-w-[260px] truncate" title={item.strategy_summary}>
        {item.strategy_summary || "--"}
      </td>
      <td
        class={`px-3 py-2 text-sm font-mono ${
          (item.pnl_pct ?? 0) >= 0 ? "text-vespra-green" : "text-vespra-red"
        }`}
      >
        {fmtPct(item.pnl_pct)}
      </td>
      <td class="px-3 py-2 text-sm font-mono text-vespra-red">
        {fmtPct(-Math.abs(item.max_drawdown_pct ?? 0))}
      </td>
      <td class="px-3 py-2 text-sm font-mono">{fmtPct(item.win_rate_pct)}</td>
      <td class="px-3 py-2 text-sm text-center">{item.total_trades ?? "--"}</td>
      <td class="px-3 py-2">
        <Badge variant={item.mode === "agents" ? "accent" : "default"}>
          {fmtMode(item.mode)}
        </Badge>
      </td>
    </tr>
  );
}

// ── Main page ───────────────────────────────────────────────

export function Backtest() {
  const [wallets, setWallets] = useState([]);
  const [history, setHistory] = useState(null);
  const [loading, setLoading] = useState(true);
  const [expandedId, setExpandedId] = useState(null);
  const [detail, setDetail] = useState(null);
  const [highlightId, setHighlightId] = useState(null);
  const tableRef = useRef(null);

  const fetchHistory = useCallback(() => {
    api.getBacktests()
      .then((data) => setHistory(Array.isArray(data) ? data : []))
      .catch(() => setHistory([]))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    fetchHistory();
    api.walletList()
      .then((data) => setWallets(Array.isArray(data) ? data : data?.wallets || []))
      .catch(() => {});
  }, [fetchHistory]);

  // Toggle row expansion — fetch full BacktestResult lazily so the index call
  // stays small. The freshly-completed backtest already comes back with the
  // full payload so we can short-circuit on it.
  const toggleRow = async (id) => {
    if (expandedId === id) {
      setExpandedId(null);
      setDetail(null);
      return;
    }
    setExpandedId(id);
    setDetail(null);
    try {
      const full = await api.getBacktest(id);
      setDetail(full);
    } catch {
      setDetail({ error: "Failed to load backtest detail" });
    }
  };

  const onCompleted = (result) => {
    fetchHistory();
    setExpandedId(result.id);
    setDetail(result);
    setHighlightId(result.id);
    // Scroll the row into view once history has refreshed.
    setTimeout(() => {
      const row = tableRef.current?.querySelector(`tr[data-bt-id="${result.id}"]`);
      if (row) row.scrollIntoView({ behavior: "smooth", block: "center" });
      // Drop highlight after a few seconds so the table doesn't permanently shout.
      setTimeout(() => setHighlightId(null), 4000);
    }, 200);
  };

  if (loading && !history) return <Loader />;

  const list = history || [];

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Backtest</h2>

      <Card title="Run Backtest">
        <BacktestForm wallets={wallets} onCompleted={onCompleted} />
      </Card>

      <Card
        title={`History (${list.length})`}
        actions={<Button variant="ghost" onClick={fetchHistory}>Refresh</Button>}
      >
        {list.length === 0 ? (
          <p class="text-sm text-vespra-muted py-6 text-center">
            No backtests yet. Run one above to see results.
          </p>
        ) : (
          <div class="overflow-x-auto -mx-4" ref={tableRef}>
            <table class="w-full text-left">
              <thead>
                <tr class="text-xs text-vespra-muted border-b border-vespra-border">
                  <th class="px-3 py-2 font-medium">Date</th>
                  <th class="px-3 py-2 font-medium">Strategy</th>
                  <th class="px-3 py-2 font-medium">P&L %</th>
                  <th class="px-3 py-2 font-medium">Max Drawdown</th>
                  <th class="px-3 py-2 font-medium">Win Rate</th>
                  <th class="px-3 py-2 font-medium text-center">Trades</th>
                  <th class="px-3 py-2 font-medium">Mode</th>
                </tr>
              </thead>
              <tbody class="divide-y divide-vespra-border/50">
                {list.map((item) => (
                  <>
                    <HistoryRow
                      key={item.id}
                      item={item}
                      expanded={expandedId === item.id}
                      highlight={highlightId === item.id}
                      onToggle={() => toggleRow(item.id)}
                    />
                    {expandedId === item.id && (
                      <tr key={`${item.id}-detail`} class="bg-vespra-bg/40">
                        <td colSpan={7} class="px-3 py-4">
                          {detail && !detail.error ? (
                            <ResultDetail result={detail} />
                          ) : detail?.error ? (
                            <p class="text-sm text-vespra-red">{detail.error}</p>
                          ) : (
                            <Loader />
                          )}
                        </td>
                      </tr>
                    )}
                  </>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
