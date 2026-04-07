import { useState, useRef, useEffect, useCallback } from "preact/hooks";
import { api } from "../lib/api.js";
import { Card, Button, Badge } from "../components/Card.jsx";

const AGENTS = [
  "coordinator", "scout", "sentinel", "risk",
  "executor", "trader", "yield", "sniper", "launcher",
];

const QUICK_ACTIONS = {
  coordinator: [
    "Summarize latest swarm activity",
    "Generate portfolio report",
    "Status update for all agents",
  ],
  scout: [
    "Find top yields on Base",
    "Scan stablecoin opportunities",
    "New high-APY protocols on Arbitrum",
  ],
  sentinel: [
    "Check all position health",
    "Monitor Aave positions",
    "Alert on depeg risk",
  ],
  risk: [
    "Evaluate Aave V3 on Base",
    "Rate protocol safety for Compound",
    "Assess bridge risks on Arbitrum",
  ],
  executor: [
    "List all wallets on sepolia",
    "Check Sepolia chain status",
    "Create a test wallet on sepolia",
  ],
  trader: [
    "Quote 0.5 WETH to USDC on Base",
    "Best DEX route for ETH to USDC",
    "Compare aggregator prices for WETH swap",
  ],
  yield: [
    "Check Aave USDC rates on Base",
    "Best lending rates for ETH",
    "Monitor health factors across protocols",
  ],
  sniper: [
    "Scan new pools on Base",
    "Find new pairs last 4 hours",
    "High TVL new launches on Arbitrum",
  ],
  launcher: [
    "Deploy test ERC-20 on Sepolia",
    "Standard token 1M supply on Base",
    "Bonding curve token plan for Base",
    "Fee-on-transfer token design",
  ],
};

const STORAGE_PREFIX = "vespra-chat-";
const MAX_MESSAGES = 50;
const MAX_AGE_DAYS = 7;

// ─── localStorage persistence ────────────────────────────────────

function loadHistory(agent) {
  try {
    const raw = localStorage.getItem(STORAGE_PREFIX + agent);
    if (!raw) return [];
    const msgs = JSON.parse(raw);
    const cutoff = Date.now() - MAX_AGE_DAYS * 86400000;
    return msgs.filter((m) => !m.ts || m.ts >= cutoff).slice(-MAX_MESSAGES);
  } catch {
    return [];
  }
}

function saveHistory(agent, msgs) {
  try {
    const trimmed = msgs.slice(-MAX_MESSAGES);
    localStorage.setItem(STORAGE_PREFIX + agent, JSON.stringify(trimmed));
  } catch {}
}

function clearHistory(agent) {
  try {
    localStorage.removeItem(STORAGE_PREFIX + agent);
  } catch {}
}

// ─── Date grouping for archive ───────────────────────────────────

function groupByDate(msgs) {
  const groups = {};
  for (const m of msgs) {
    if (!m.ts) continue;
    const d = new Date(m.ts).toLocaleDateString("en-US", {
      weekday: "short", month: "short", day: "numeric",
    });
    if (!groups[d]) groups[d] = [];
    groups[d].push(m);
  }
  return groups;
}

// ─── Response Renderers ──────────────────────────────────────────

function riskVariant(score) {
  if (!score) return "default";
  const s = score.toString().toUpperCase();
  if (s === "LOW") return "green";
  if (s === "MEDIUM") return "yellow";
  if (s === "HIGH") return "orange";
  if (s === "CRITICAL") return "red";
  return "default";
}

function apyColor(apy) {
  const n = parseFloat(apy);
  if (isNaN(n)) return "text-vespra-text";
  if (n >= 20) return "text-vespra-green";
  if (n >= 8) return "text-vespra-yellow";
  return "text-vespra-red";
}

function formatTvl(tvl) {
  if (!tvl) return "-";
  if (typeof tvl === "string" && tvl.startsWith("$")) return tvl;
  const n = parseFloat(tvl);
  if (isNaN(n)) return tvl;
  if (n >= 1e9) return `$${(n / 1e9).toFixed(1)}B`;
  if (n >= 1e6) return `$${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `$${(n / 1e3).toFixed(0)}K`;
  return `$${n.toFixed(0)}`;
}

function typeLabel(type) {
  const labels = {
    lending_supply: "Lending",
    lp_concentrated: "CL LP",
    lp_stable: "Stable LP",
    lp_volatile: "LP",
    staking: "Staking",
    vault: "Vault",
  };
  return labels[type] || type || "-";
}

function ScoutView({ data }) {
  const items = Array.isArray(data) ? data : [data];
  return (
    <div class="space-y-3">
      {items.map((r, i) => (
        <div key={i} class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-2">
          <div class="flex items-center justify-between">
            <div class="flex items-center gap-2">
              {r.url ? (
                <a href={r.url} target="_blank" rel="noopener" class="font-medium text-vespra-accent hover:text-vespra-accent-glow underline underline-offset-2">
                  {r.protocol || "Unknown"}
                </a>
              ) : (
                <span class="font-medium text-vespra-text">{r.protocol || "Unknown"}</span>
              )}
              <Badge variant="accent">{r.chain || "?"}</Badge>
              <Badge variant="default">{typeLabel(r.type)}</Badge>
            </div>
            <span class={`text-base font-bold ${apyColor(r.apy)}`}>{r.apy || "-"}</span>
          </div>
          {r.pair && (
            <div class="flex justify-between">
              <span class="text-vespra-muted">Pair</span>
              <span class="font-mono text-vespra-text">{r.pair}</span>
            </div>
          )}
          {r.pool_address && /^0x[0-9a-fA-F]{40}$/.test(r.pool_address) && (
            <div class="flex justify-between">
              <span class="text-vespra-muted">Pool</span>
              <span class="font-mono text-vespra-muted">{r.pool_address.slice(0, 10)}...{r.pool_address.slice(-4)}</span>
            </div>
          )}
          <div class="flex justify-between">
            <span class="text-vespra-muted">TVL</span>
            <span>{formatTvl(r.tvl)}</span>
          </div>
          {r.strategy_detail && (
            <p class="text-vespra-text border-t border-vespra-border/50 pt-2">{r.strategy_detail}</p>
          )}
          {r.risk_notes && (
            <p class="text-vespra-yellow text-[11px]">Risk: {r.risk_notes}</p>
          )}
        </div>
      ))}
    </div>
  );
}

function RiskView({ data }) {
  const items = Array.isArray(data) ? data : [data];
  return (
    <div class="space-y-3">
      {items.map((r, i) => (
        <div key={i} class="bg-vespra-bg rounded-lg border border-vespra-border p-4">
          <div class="flex items-center justify-between mb-3">
            <div class="flex items-center gap-2">
              <span class="text-sm font-bold text-vespra-text">{r.protocol || "Protocol"}</span>
              {r.chain && <Badge variant="accent">{r.chain}</Badge>}
            </div>
            <Badge variant={riskVariant(r.score || r.overall_score)}>{r.score || r.overall_score || "?"}</Badge>
          </div>
          {r.factors && Array.isArray(r.factors) && r.factors.length > 0 && (
            <div class="space-y-1.5 mb-3 border-t border-vespra-border/50 pt-3">
              {r.factors.map((f, j) => (
                <div key={j} class="flex items-start gap-2 text-xs">
                  <Badge variant={riskVariant(f.rating || f.score)}>{f.rating || f.score || "?"}</Badge>
                  <span class="text-vespra-muted shrink-0">{f.category || f.name}:</span>
                  <span class="text-vespra-text">{f.detail || f.description || f.notes}</span>
                </div>
              ))}
            </div>
          )}
          {(r.recommendation || r.summary) && (
            <p class="text-xs text-vespra-yellow/80 border-t border-vespra-border/50 pt-2 mt-2">
              {r.recommendation || r.summary}
            </p>
          )}
        </div>
      ))}
    </div>
  );
}

function SentinelView({ data }) {
  const items = Array.isArray(data) ? data : [data];
  return (
    <div class="space-y-2">
      {items.map((a, i) => (
        <div key={i} class="bg-vespra-bg rounded border border-vespra-border p-3 flex items-start gap-3">
          <Badge variant={riskVariant(a.severity)}>{a.severity || "?"}</Badge>
          <div class="flex-1 text-xs">
            <div class="flex items-center gap-2 mb-1">
              <span class="font-medium text-vespra-text">{a.protocol || "Unknown"}</span>
              {a.alert_type && <span class="text-vespra-muted">({a.alert_type})</span>}
            </div>
            {a.position && <p class="text-vespra-muted">Position: {a.position}</p>}
            {a.details && <p class="text-vespra-text mt-1">{a.details}</p>}
            {a.recommended_action && <p class="text-vespra-accent mt-1">{a.recommended_action}</p>}
          </div>
        </div>
      ))}
    </div>
  );
}

function formatTokenAmount(raw, decimals) {
  if (!raw) return "?";
  const s = String(raw);
  const d = parseInt(decimals) || 18;
  // If it looks like a decimal already, return as-is
  if (s.includes(".") || s.length <= 6) return s;
  // Convert from wei-like units
  const n = parseFloat(s) / Math.pow(10, d);
  if (n === 0) return "0";
  if (n >= 1) return n.toFixed(4).replace(/\.?0+$/, "");
  return n.toPrecision(4).replace(/0+$/, "");
}

function CopyText({ text, label }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <button onClick={copy} title="Copy" class="inline-flex items-center gap-1 hover:text-vespra-accent-glow transition-colors cursor-pointer">
      <span class="break-all">{label}</span>
      {copied
        ? <span class="text-vespra-green text-[10px] shrink-0">copied</span>
        : <svg class="w-3 h-3 shrink-0 text-vespra-muted" viewBox="0 0 16 16" fill="currentColor"><path d="M0 6.75C0 5.784.784 5 1.75 5h1.5a.75.75 0 010 1.5h-1.5a.25.25 0 00-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 00.25-.25v-1.5a.75.75 0 011.5 0v1.5A1.75 1.75 0 019.25 16h-7.5A1.75 1.75 0 010 14.25zM5 1.75C5 .784 5.784 0 6.75 0h7.5C15.216 0 16 .784 16 1.75v7.5A1.75 1.75 0 0114.25 11h-7.5A1.75 1.75 0 015 9.25zm1.75-.25a.25.25 0 00-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 00.25-.25v-7.5a.25.25 0 00-.25-.25z"/></svg>
      }
    </button>
  );
}

function truncateAddress(addr) {
  if (!addr || addr.length < 16) return addr;
  return addr.slice(0, 10) + "..." + addr.slice(-4);
}

function TraderView({ data }) {
  const swap = data.swap || data;
  const status = data.status || swap.status;
  const chain = swap.chain || data.chain;
  const slippage = swap.slippage_bps ?? data.slippage_bps;
  const aggregator = swap.aggregator || data.aggregator;
  const router = swap.router_address || data.router_address;
  const instruction = data.executor_instruction || swap.executor_instruction;
  const [showCalldata, setShowCalldata] = useState(false);
  const [showInstruction, setShowInstruction] = useState(false);

  const hasTokenPair = swap.token_in && swap.token_out;
  const amountIn = hasTokenPair ? formatTokenAmount(swap.amount_in, swap.token_in.decimals) : null;
  const amountOut = hasTokenPair ? formatTokenAmount(swap.expected_out, swap.token_out.decimals) : null;
  const symbolIn = hasTokenPair ? (swap.token_in.symbol || "?") : null;
  const symbolOut = hasTokenPair ? (swap.token_out.symbol || "?") : null;

  // Calldata detection in instruction
  const hasCalldata = instruction && /0x[0-9a-fA-F]{40,}/.test(instruction);
  // Strip calldata from instruction text for display
  const instructionText = instruction
    ? instruction.replace(/0x[0-9a-fA-F]{40,}/g, "").replace(/\s{2,}/g, " ").trim()
    : null;
  const calldataMatch = instruction ? instruction.match(/0x[0-9a-fA-F]{40,}/) : null;
  const calldata = calldataMatch ? calldataMatch[0] : null;

  // Build details grid — only fields with values
  const details = [];
  if (chain) details.push({ label: "Chain", value: <Badge variant="accent">{chain}</Badge> });
  if (aggregator) details.push({ label: "Aggregator", value: <span class="text-vespra-text">{aggregator}</span> });
  if (slippage != null) details.push({ label: "Slippage", value: <span class="text-vespra-text">{(slippage / 100).toFixed(2)}%</span> });
  if (router) details.push({
    label: "Router",
    value: <span class="font-mono text-vespra-muted"><CopyText text={router} label={truncateAddress(router)} /></span>,
  });

  return (
    <div class="bg-vespra-bg rounded-lg border border-vespra-border overflow-hidden">
      {/* Header with status badge */}
      <div class="flex items-center justify-between px-4 pt-3 pb-1">
        <span class="text-[11px] text-vespra-muted uppercase tracking-wide font-medium">Swap Preview</span>
        {status && (
          <Badge variant={status === "ready" ? "green" : status === "no_route" ? "red" : "yellow"}>
            {status}
          </Badge>
        )}
      </div>

      {/* Token pair hero */}
      {hasTokenPair ? (
        <div class="px-4 py-4 flex items-center justify-center gap-3">
          <div class="text-right">
            <div class="text-lg font-bold text-vespra-text leading-tight">{amountIn}</div>
            <div class="text-sm font-semibold text-vespra-accent">{symbolIn}</div>
          </div>
          <div class="flex items-center justify-center w-8 h-8 rounded-full bg-vespra-border">
            <svg class="w-4 h-4 text-vespra-muted" viewBox="0 0 16 16" fill="currentColor">
              <path d="M1 8a.5.5 0 01.5-.5h11.793l-3.147-3.146a.5.5 0 01.708-.708l4 4a.5.5 0 010 .708l-4 4a.5.5 0 01-.708-.708L13.293 8.5H1.5A.5.5 0 011 8z"/>
            </svg>
          </div>
          <div class="text-left">
            <div class="text-lg font-bold text-vespra-text leading-tight">{amountOut}</div>
            <div class="text-sm font-semibold text-vespra-green">{symbolOut}</div>
          </div>
        </div>
      ) : (
        <div class="px-4 py-3 text-center text-sm text-vespra-muted">No token pair data</div>
      )}

      {/* Details grid */}
      {details.length > 0 && (
        <div class="border-t border-vespra-border/50 mx-4">
          <div class="grid grid-cols-2 gap-x-4 gap-y-2 py-3 text-xs">
            {details.map((d, i) => (
              <div key={i} class="flex justify-between items-center col-span-2 sm:col-span-1">
                <span class="text-vespra-muted">{d.label}</span>
                {d.value}
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Calldata toggle */}
      {calldata && (
        <div class="border-t border-vespra-border/50 mx-4">
          <button
            onClick={() => setShowCalldata(!showCalldata)}
            class="flex items-center gap-1.5 py-2 text-[11px] text-vespra-muted hover:text-vespra-text transition-colors w-full"
          >
            <svg class={`w-3 h-3 transition-transform ${showCalldata ? "rotate-90" : ""}`} viewBox="0 0 16 16" fill="currentColor">
              <path d="M6 3l5 5-5 5V3z" />
            </svg>
            View calldata
          </button>
          {showCalldata && (
            <div class="pb-3">
              <div class="bg-vespra-border/50 rounded p-2 font-mono text-[10px] text-vespra-muted break-all max-h-32 overflow-y-auto">
                {calldata}
              </div>
            </div>
          )}
        </div>
      )}

      {/* Executor instruction — collapsible */}
      {instructionText && (
        <div class="border-t border-vespra-border/50 mx-4">
          <button
            onClick={() => setShowInstruction(!showInstruction)}
            class="flex items-center gap-1.5 py-2 text-[11px] text-vespra-muted hover:text-vespra-text transition-colors w-full"
          >
            <svg class={`w-3 h-3 transition-transform ${showInstruction ? "rotate-90" : ""}`} viewBox="0 0 16 16" fill="currentColor">
              <path d="M6 3l5 5-5 5V3z" />
            </svg>
            Executor instruction
          </button>
          {showInstruction && (
            <p class="text-xs text-vespra-accent pb-3 break-words whitespace-pre-wrap">{instructionText}</p>
          )}
        </div>
      )}

      {/* Bottom padding */}
      <div class="h-1" />
    </div>
  );
}

function CopyAddress({ address }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    navigator.clipboard.writeText(address).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };
  return (
    <button
      onClick={copy}
      title="Click to copy"
      class="hover:text-vespra-accent-glow transition-colors cursor-pointer text-left break-all"
    >
      {address}{copied && <span class="ml-1 text-vespra-green text-[10px]">copied</span>}
    </button>
  );
}

function ExecutorWalletTable({ wallets }) {
  return (
    <table class="w-full text-xs mt-1">
      <thead>
        <tr class="text-left text-vespra-muted border-b border-vespra-border/50">
          <th scope="col" class="py-1 pr-2 font-medium">Label</th>
          <th scope="col" class="py-1 pr-2 font-medium">Chain</th>
          <th scope="col" class="py-1 pr-2 font-medium">Address</th>
          <th scope="col" class="py-1 font-medium">Status</th>
        </tr>
      </thead>
      <tbody>
        {wallets.map((w, i) => (
          <tr key={i} class="border-b border-vespra-border/30">
            <td class="py-1 pr-2 text-vespra-text">{w.label || "-"}</td>
            <td class="py-1 pr-2"><Badge variant="accent">{w.chain || "?"}</Badge></td>
            <td class="py-1 pr-2 font-mono text-vespra-accent">
              <CopyAddress address={w.address} />
            </td>
            <td class="py-1">
              <Badge variant={w.active ? "green" : "red"}>{w.active ? "active" : "inactive"}</Badge>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ExecutorView({ data }) {
  const results = data.keymaster_results || [];
  const warnings = data.warnings || [];
  return (
    <div class="space-y-2">
      {data.status && (
        <Badge variant={data.status === "ok" ? "green" : "red"}>{data.status}</Badge>
      )}
      {results.map((r, i) => {
        const resp = r.result?.response;
        const isWalletList = Array.isArray(resp) && resp.length > 0 && resp[0]?.address;
        return (
          <div key={i} class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-1">
            <div class="flex justify-between">
              <span class="font-medium text-vespra-text">{r.action}</span>
              <Badge variant={r.result?.status === "error" ? "red" : "green"}>
                {r.result?.status || "?"}
              </Badge>
            </div>
            {isWalletList && (
              <ExecutorWalletTable wallets={resp} />
            )}
            {!isWalletList && resp?.wallet_id && (
              <div class="flex justify-between text-vespra-muted">
                <span>Wallet</span>
                <span class="font-mono">{resp.wallet_id}</span>
              </div>
            )}
            {!isWalletList && resp?.tx_hash && (
              <div class="flex justify-between text-vespra-muted">
                <span>TX Hash</span>
                <span class="font-mono text-vespra-accent">{resp.tx_hash}</span>
              </div>
            )}
            {!isWalletList && resp?.address && (
              <div class="flex justify-between text-vespra-muted">
                <span>Address</span>
                <span class="font-mono text-vespra-accent">{resp.address}</span>
              </div>
            )}
            {!isWalletList && resp?.balance_eth && (
              <div class="flex justify-between text-vespra-muted">
                <span>Balance</span>
                <span>{resp.balance_eth} ETH</span>
              </div>
            )}
            {r.result?.error && (
              <p class="text-vespra-red">{r.result.error}</p>
            )}
          </div>
        );
      })}
      {warnings.length > 0 && (
        <div class="text-xs space-y-1">
          {warnings.map((w, i) => (
            <p key={i} class="text-vespra-yellow">Warning: {w}</p>
          ))}
        </div>
      )}
    </div>
  );
}

function YieldView({ data }) {
  const action = data.recommended_action || data.action || null;
  return (
    <div class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-2">
      {action && (
        <div class="space-y-1">
          <div class="flex justify-between">
            <span class="text-vespra-muted">Action</span>
            <Badge variant={action === "hold" ? "yellow" : action === "deposit" || action === "rebalance" ? "green" : "default"}>
              {action}
            </Badge>
          </div>
          {data.reasoning && (
            <div class="text-vespra-muted text-xs">{data.reasoning}</div>
          )}
        </div>
      )}
      {data.protocol && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">Protocol</span>
          <span>{data.protocol}</span>
        </div>
      )}
      {data.chain && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">Chain</span>
          <Badge variant="accent">{data.chain}</Badge>
        </div>
      )}
      {data.position && (
        <>
          {data.position.asset && (
            <div class="flex justify-between">
              <span class="text-vespra-muted">Asset</span>
              <span>{data.position.asset} — {data.position.amount || "?"}</span>
            </div>
          )}
          {data.position.supply_apy && (
            <div class="flex justify-between">
              <span class="text-vespra-muted">Supply APY</span>
              <span class={apyColor(data.position.supply_apy)}>{data.position.supply_apy}</span>
            </div>
          )}
          {data.position.health_factor && (
            <div class="flex justify-between">
              <span class="text-vespra-muted">Health Factor</span>
              <span class={parseFloat(data.position.health_factor) < 1.5 ? "text-vespra-red" : "text-vespra-green"}>
                {data.position.health_factor}
              </span>
            </div>
          )}
        </>
      )}
      {data.executor_instruction && (
        <p class="text-vespra-accent border-t border-vespra-border pt-2">{data.executor_instruction}</p>
      )}
      {data.warnings?.length > 0 && data.warnings.map((w, i) => (
        <p key={i} class="text-vespra-yellow">Warning: {w}</p>
      ))}
    </div>
  );
}

function SniperView({ data }) {
  if (data.status === "pass" || data.status === "no_opportunity") {
    return (
      <div class="bg-vespra-bg rounded border border-vespra-border p-4 text-center">
        <div class="text-vespra-muted text-sm mb-1">No qualifying pools found</div>
        {data.reason && <div class="text-xs text-vespra-muted">{data.reason}</div>}
        {data.filters && <div class="text-xs text-vespra-muted mt-1">Filters: {typeof data.filters === "string" ? data.filters : JSON.stringify(data.filters)}</div>}
      </div>
    );
  }

  const pool = data.pool || {};
  return (
    <div class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-2">
      <div class="flex justify-between">
        <span class="text-vespra-muted">Status</span>
        <Badge variant={data.status === "opportunity" ? "green" : "red"}>
          {data.status || "?"}
        </Badge>
      </div>
      {pool.pair && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">Pair</span>
          <span class="font-medium text-vespra-text">{pool.pair}</span>
        </div>
      )}
      {pool.dex && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">DEX</span>
          <span>{pool.dex}</span>
        </div>
      )}
      {pool.chain && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">Chain</span>
          <Badge variant="accent">{pool.chain}</Badge>
        </div>
      )}
      {pool.tvl_usd && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">TVL</span>
          <span>{pool.tvl_usd}</span>
        </div>
      )}
      {data.risk_assessment && (
        <div class="flex justify-between">
          <span class="text-vespra-muted">Risk</span>
          <Badge variant={riskVariant(data.risk_assessment.score)}>{data.risk_assessment.score || "?"}</Badge>
        </div>
      )}
      {data.entry?.executor_instruction && (
        <p class="text-vespra-accent border-t border-vespra-border pt-2">{data.entry.executor_instruction}</p>
      )}
    </div>
  );
}

function formatSupply(val) {
  if (!val) return "-";
  const s = String(val).replace(/,/g, "");
  const n = parseFloat(s);
  if (isNaN(n)) return val;
  if (n >= 1e12) return `${(n / 1e12).toFixed(1).replace(/\.0$/, "")}T`;
  if (n >= 1e9) return `${(n / 1e9).toFixed(1).replace(/\.0$/, "")}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1).replace(/\.0$/, "")}M`;
  if (n >= 1e3) return n.toLocaleString("en-US", { maximumFractionDigits: 0 });
  return s;
}

function LauncherView({ data }) {
  const tc = data.token_config || {};
  const dep = data.deployment || {};
  const liq = data.liquidity || {};
  return (
    <div class="space-y-2">
      <div class="flex justify-between">
        <Badge variant={data.status === "planned" ? "green" : "red"}>{data.status || "?"}</Badge>
      </div>
      <div class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-1">
        <div class="text-sm font-medium text-vespra-text mb-1">{tc.name || "?"} ({tc.symbol || "?"})</div>
        {tc.chain && <div class="flex justify-between"><span class="text-vespra-muted">Chain</span><Badge variant="accent">{tc.chain}</Badge></div>}
        {tc.total_supply && <div class="flex justify-between"><span class="text-vespra-muted">Supply</span><span>{formatSupply(tc.total_supply)}</span></div>}
        {dep.contract_type && <div class="flex justify-between"><span class="text-vespra-muted">Type</span><span>{dep.contract_type}</span></div>}
        {dep.estimated_gas && <div class="flex justify-between"><span class="text-vespra-muted">Est. Gas</span><span>{dep.estimated_gas}</span></div>}
        {dep.wallet_id && <div class="flex justify-between"><span class="text-vespra-muted">Deployer</span><span class="font-mono">{dep.wallet_id}</span></div>}
      </div>
      {liq.dex && liq.dex !== "none" && (
        <div class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-1">
          <div class="text-vespra-muted font-medium mb-1">Liquidity</div>
          <div class="flex justify-between"><span class="text-vespra-muted">DEX</span><span>{liq.dex}</span></div>
          {liq.pair_token && <div class="flex justify-between"><span class="text-vespra-muted">Pair</span><span>{liq.pair_token}</span></div>}
          {liq.lock_duration_days > 0 && <div class="flex justify-between"><span class="text-vespra-muted">Lock</span><span>{liq.lock_duration_days} days</span></div>}
        </div>
      )}
      {data.warnings?.length > 0 && (
        <div class="text-xs space-y-1">
          {data.warnings.map((w, i) => <p key={i} class="text-vespra-yellow">Warning: {w}</p>)}
        </div>
      )}
    </div>
  );
}

function GenericJsonView({ data }) {
  if (typeof data !== "object") return <pre class="text-xs text-vespra-text whitespace-pre-wrap">{String(data)}</pre>;
  return (
    <div class="bg-vespra-bg rounded border border-vespra-border p-3 overflow-x-auto">
      <pre class="text-xs text-vespra-muted whitespace-pre-wrap">{JSON.stringify(data, null, 2)}</pre>
    </div>
  );
}

// ─── Render agent response ───────────────────────────────────────

function extractJson(text) {
  if (typeof text !== "string") return text;
  text = text.trim();
  try { return JSON.parse(text); } catch {}
  // Try markdown fences
  const fenceMatch = text.match(/```(?:json)?\s*([\s\S]*?)```/);
  if (fenceMatch) {
    try { return JSON.parse(fenceMatch[1].trim()); } catch {}
  }
  // Try first { to last }
  const first = text.indexOf("{");
  const last = text.lastIndexOf("}");
  if (first !== -1 && last > first) {
    try { return JSON.parse(text.slice(first, last + 1)); } catch {}
  }
  // Try first [ to last ]
  const firstArr = text.indexOf("[");
  const lastArr = text.lastIndexOf("]");
  if (firstArr !== -1 && lastArr > firstArr) {
    try { return JSON.parse(text.slice(firstArr, lastArr + 1)); } catch {}
  }
  return null;
}

function parseMarkdown(text) {
  if (typeof text !== "string") return String(text);
  let html = text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>")
    .replace(/\*(.+?)\*/g, "<em>$1</em>");
  // List items
  html = html.replace(/(^|\n)([-*] .+(?:\n[-*] .+)*)/g, (_, pre, block) => {
    const items = block.split("\n").map((l) => `<li>${l.replace(/^[-*] /, "")}</li>`).join("");
    return `${pre}<ul class="list-disc pl-4 my-1">${items}</ul>`;
  });
  // Headings
  html = html.replace(/(^|\n)## (.+)/g, '$1<h3 class="font-bold text-base mt-2 mb-1">$2</h3>');
  // Double newlines
  html = html.replace(/\n\n/g, "<br><br>");
  return html;
}

function AgentResponse({ content, agent }) {
  let parsed = typeof content === "string" ? extractJson(content) : content;

  if (parsed == null) {
    const trimmed = typeof content === "string" ? content.trim() : "";
    if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
      try {
        const formatted = JSON.stringify(JSON.parse(trimmed), null, 2);
        return (
          <pre class="text-xs text-vespra-muted bg-vespra-bg rounded border border-vespra-border p-3 overflow-x-auto whitespace-pre-wrap">{formatted}</pre>
        );
      } catch {}
    }
    return <div class="text-sm text-vespra-text whitespace-pre-wrap" dangerouslySetInnerHTML={{ __html: parseMarkdown(content) }} />;
  }

  if (typeof parsed === "string") {
    const inner = extractJson(parsed);
    if (inner != null) parsed = inner;
  }

  if (agent === "scout" && (Array.isArray(parsed) || parsed?.protocol))
    return <ScoutView data={parsed} />;
  if (agent === "risk") {
    // Unwrap common wrapper keys
    const riskData = parsed?.risk_assessment || parsed?.assessment || parsed?.risk || parsed?.assessments || parsed;
    // Only render RiskView when we have structured data with a factors array
    if (Array.isArray(riskData?.factors) && riskData.factors.length > 0)
      return <RiskView data={riskData} />;
  }
  if ((agent === "coordinator" || agent === "sentinel") && parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const prose = parsed.message || parsed.note;
    if (prose && typeof prose === "string") {
      return <div class="text-sm text-vespra-text whitespace-pre-wrap" dangerouslySetInnerHTML={{ __html: parseMarkdown(prose) }} />;
    }
  }
  if (agent === "executor" && parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const prose = parsed.note || parsed.message;
    if (prose && typeof prose === "string") {
      return <div class="text-sm text-vespra-text whitespace-pre-wrap" dangerouslySetInnerHTML={{ __html: parseMarkdown(prose) }} />;
    }
  }
  if (agent === "sentinel" && parsed?.severity && parsed?.alert_type)
    return <SentinelView data={parsed} />;
  if (agent === "trader" && (parsed?.swap || parsed?.status === "ready" || parsed?.status === "no_route"))
    return <TraderView data={parsed} />;
  if (agent === "trader" && parsed?.action && (parsed?.action_type || parsed?.token_pair || parsed?.token_in || parsed?.amount || parsed?.reasoning)) {
    const actionUpper = (parsed.action || "").toUpperCase();
    const actionColor = actionUpper === "BUY" ? "text-vespra-green" : actionUpper === "SELL" ? "text-vespra-red" : "text-vespra-yellow";
    return (
      <div class="bg-vespra-bg rounded border border-vespra-border p-3 text-xs space-y-2">
        <div class="flex items-center justify-between">
          <span class={`font-mono font-bold text-base uppercase ${actionColor}`}>{actionUpper}</span>
          {parsed.expected_gain_pct > 0 && (
            <span class="text-vespra-green text-xs">+{parsed.expected_gain_pct}%</span>
          )}
        </div>
        {(parsed.token_pair || parsed.token_in) && (
          <div class="flex justify-between">
            <span class="text-vespra-muted">Pair</span>
            <span class="font-mono text-vespra-text">{parsed.token_pair || `${parsed.token_in}${parsed.token_out ? ` → ${parsed.token_out}` : ""}`}</span>
          </div>
        )}
        {parsed.amount && (
          <div class="flex justify-between">
            <span class="text-vespra-muted">Amount</span>
            <span class="font-mono text-vespra-text">{parsed.amount}</span>
          </div>
        )}
        {parsed.reasoning && (
          <div class="text-vespra-muted border-t border-vespra-border pt-2 mt-1">{parsed.reasoning}</div>
        )}
      </div>
    );
  }
  if (agent === "executor" && (parsed?.keymaster_results || parsed?.keymaster_calls))
    return <ExecutorView data={parsed} />;
  if (agent === "yield" && (parsed?.position || parsed?.protocol || parsed?.action || parsed?.recommended_action))
    return <YieldView data={parsed} />;
  if (agent === "sniper" && (parsed?.pool || parsed?.risk_assessment))
    return <SniperView data={parsed} />;
  if (agent === "launcher" && (parsed?.token_config || parsed?.deployment))
    return <LauncherView data={parsed} />;

  // Universal fallback: any agent response that's just a {message: "..."} or
  // {note: "..."} wrapper should render as prose, not raw JSON. The
  // coordinator/sentinel/executor branches above handle the same case earlier
  // so they keep their existing precedence; this catches every other agent.
  if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
    const prose = parsed.message || parsed.note;
    if (prose && typeof prose === "string") {
      return (
        <div
          class="text-sm text-vespra-text whitespace-pre-wrap"
          dangerouslySetInnerHTML={{ __html: parseMarkdown(prose) }}
        />
      );
    }
  }

  return <GenericJsonView data={parsed} />;
}

// ─── Message bubble ──────────────────────────────────────────────

function Message({ msg, agent }) {
  const isUser = msg.role === "user";
  if (isUser) {
    return (
      <div class="flex justify-end">
        <div class="max-w-[85%] rounded-lg px-3 py-2 text-sm whitespace-pre-wrap bg-vespra-accent/15 text-vespra-accent">
          {msg.content}
        </div>
      </div>
    );
  }
  return (
    <div class="flex justify-start">
      <div class="max-w-[90%] rounded-lg px-3 py-2 bg-vespra-border">
        <AgentResponse content={msg.content} agent={agent} />
      </div>
    </div>
  );
}

// ─── Thinking indicator ──────────────────────────────────────────

function ThinkingIndicator({ agent }) {
  const [dots, setDots] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setDots((d) => (d + 1) % 4), 500);
    return () => clearInterval(id);
  }, []);
  return (
    <div class="flex justify-start">
      <div class="rounded-lg px-3 py-2 bg-vespra-border">
        <div class="flex items-center gap-2 text-sm text-vespra-muted">
          <div class="flex gap-1">
            <span class={`w-1.5 h-1.5 rounded-full bg-vespra-accent ${dots >= 1 ? "opacity-100" : "opacity-20"} transition-opacity`} />
            <span class={`w-1.5 h-1.5 rounded-full bg-vespra-accent ${dots >= 2 ? "opacity-100" : "opacity-20"} transition-opacity`} />
            <span class={`w-1.5 h-1.5 rounded-full bg-vespra-accent ${dots >= 3 ? "opacity-100" : "opacity-20"} transition-opacity`} />
          </div>
          <span class="capitalize">{agent}</span> is thinking...
        </div>
      </div>
    </div>
  );
}

// ─── Chat archive (collapsible history by date) ──────────────────

function ChatArchive({ messages, agent }) {
  const [open, setOpen] = useState(false);

  // Only show messages older than the current "session" (before last clear / page load)
  // We use date grouping on all stored messages
  const groups = groupByDate(messages);
  const dates = Object.keys(groups);

  if (dates.length === 0) return null;

  return (
    <div class="mb-3">
      <button
        onClick={() => setOpen(!open)}
        class="flex items-center gap-1.5 text-xs text-vespra-muted hover:text-vespra-text transition-colors"
      >
        <svg class={`w-3 h-3 transition-transform ${open ? "rotate-90" : ""}`} viewBox="0 0 16 16" fill="currentColor">
          <path d="M6 3l5 5-5 5V3z" />
        </svg>
        History ({messages.length} messages)
      </button>
      {open && (
        <div class="mt-2 space-y-3 max-h-60 overflow-y-auto border-l-2 border-vespra-border pl-3">
          {dates.map((date) => (
            <div key={date}>
              <div class="text-[10px] text-vespra-muted font-medium mb-1">{date}</div>
              <div class="space-y-1.5">
                {groups[date].map((msg, i) => (
                  <div key={i} class={`text-xs truncate ${msg.role === "user" ? "text-vespra-accent" : "text-vespra-muted"}`}>
                    {msg.role === "user" ? "> " : "< "}
                    {typeof msg.content === "string" ? msg.content.slice(0, 120) : "[response]"}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ─── Agent Config Panel ─────────────────────────────────────────

const BASE_GW = import.meta.env.MODE === "production"
  ? "https://api.vespra.xyz"
  : "http://127.0.0.1:9001";

const AGENT_CONFIG_FIELDS = {
  scout: [
    { key: "scout_chains", label: "Chains to scan", type: "multiselect", options: ["base", "arbitrum", "optimism", "ethereum"] },
    { key: "scout_min_tvl_usd", label: "Min TVL ($)", type: "number" },
    { key: "scout_protocols_whitelist", label: "Protocol whitelist (blank = all)", type: "text" },
  ],
  risk: [
    { key: "risk_max_score", label: "Max risk score", type: "number", min: 0, max: 100 },
    { key: "risk_blacklisted_protocols", label: "Blacklisted protocols", type: "text" },
  ],
  trader: [
    { key: "trader_max_slippage_pct", label: "Max slippage (%)", type: "number", step: 0.1 },
    { key: "trader_gas_multiplier", label: "Gas multiplier", type: "number", step: 0.05 },
  ],
  sentinel: [
    { key: "sentinel_alert_severity_min", label: "Min alert severity", type: "select", options: ["low", "medium", "high"] },
    { key: "sentinel_poll_interval_secs", label: "Poll interval (seconds)", type: "number" },
  ],
  yield: [
    { key: "yield_target_apy_floor", label: "Target APY floor (%)", type: "number" },
    { key: "yield_max_position_eth", label: "Max position (ETH)", type: "number" },
  ],
  sniper: [
    { key: "sniper_min_pool_liquidity_usd", label: "Min pool liquidity ($)", type: "number" },
    { key: "sniper_max_pool_age_blocks", label: "Max pool age (blocks)", type: "number" },
  ],
  launcher: [
    { key: "launcher_default_decimals", label: "Default decimals", type: "number" },
    { key: "launcher_default_supply", label: "Default supply", type: "number" },
    { key: "launcher_default_chain", label: "Default chain", type: "select", options: ["base", "arbitrum", "optimism", "ethereum"] },
  ],
};

function AgentConfigPanel({ agent }) {
  const fields = AGENT_CONFIG_FIELDS[agent];
  if (!fields) return null;

  const [open, setOpen] = useState(false);
  const [values, setValues] = useState({});
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    setOpen(false);
    setLoaded(false);
    setSaved(false);
  }, [agent]);

  useEffect(() => {
    if (!open || loaded) return;
    fetch(`${BASE_GW}/config`)
      .then((r) => r.json())
      .then((data) => {
        const init = {};
        for (const f of fields) {
          if (data[f.key] !== undefined) {
            init[f.key] = f.type === "multiselect" && Array.isArray(data[f.key])
              ? data[f.key]
              : f.type === "text" && Array.isArray(data[f.key])
              ? data[f.key].join(", ")
              : data[f.key];
          }
        }
        setValues(init);
        setLoaded(true);
      })
      .catch(() => setLoaded(true));
  }, [open, loaded, agent]);

  const updateField = (key, value) => {
    setValues((v) => ({ ...v, [key]: value }));
    setSaved(false);
  };

  const toggleMulti = (key, option) => {
    setValues((v) => {
      const arr = v[key] || [];
      return { ...v, [key]: arr.includes(option) ? arr.filter((x) => x !== option) : [...arr, option] };
    });
    setSaved(false);
  };

  const saveConfig = async () => {
    setSaving(true);
    const patch = {};
    for (const f of fields) {
      const val = values[f.key];
      if (val === undefined) continue;
      if (f.type === "text" && typeof val === "string") {
        patch[f.key] = val.trim() ? val.split(",").map((s) => s.trim()) : [];
      } else {
        patch[f.key] = val;
      }
    }
    try {
      await fetch(`${BASE_GW}/config`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(patch),
      });
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch {}
    setSaving(false);
  };

  return (
    <div class="mb-3">
      <button
        onClick={() => setOpen(!open)}
        class="flex items-center gap-1.5 text-xs text-vespra-muted hover:text-vespra-text transition-colors"
      >
        <svg class={`w-3 h-3 transition-transform ${open ? "rotate-90" : ""}`} viewBox="0 0 16 16" fill="currentColor">
          <path d="M6 3l5 5-5 5V3z" />
        </svg>
        Settings
      </button>
      {open && (
        <div class="mt-2 p-3 bg-vespra-bg rounded border border-vespra-border space-y-3">
          {fields.map((f) => (
            <div key={f.key}>
              <label class="text-xs text-vespra-muted block mb-1">{f.label}</label>
              {f.type === "number" && (
                <input
                  type="number"
                  value={values[f.key] ?? ""}
                  onInput={(e) => updateField(f.key, e.target.value === "" ? "" : Number(e.target.value))}
                  step={f.step}
                  min={f.min}
                  max={f.max}
                  class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text focus:border-vespra-accent w-full"
                />
              )}
              {f.type === "text" && (
                <input
                  type="text"
                  value={values[f.key] ?? ""}
                  onInput={(e) => updateField(f.key, e.target.value)}
                  placeholder="comma-separated"
                  class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text placeholder:text-vespra-muted focus:border-vespra-accent w-full"
                />
              )}
              {f.type === "select" && (
                <select
                  value={values[f.key] ?? ""}
                  onChange={(e) => updateField(f.key, e.target.value)}
                  class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text focus:border-vespra-accent w-full"
                >
                  <option value="">--</option>
                  {f.options.map((o) => (
                    <option key={o} value={o}>{o}</option>
                  ))}
                </select>
              )}
              {f.type === "multiselect" && (
                <div class="flex gap-3 flex-wrap">
                  {f.options.map((o) => (
                    <label key={o} class="flex items-center gap-1.5 cursor-pointer">
                      <input
                        type="checkbox"
                        checked={(values[f.key] || []).includes(o)}
                        onChange={() => toggleMulti(f.key, o)}
                        class="accent-vespra-accent"
                      />
                      <span class="text-sm text-vespra-text">{o}</span>
                    </label>
                  ))}
                </div>
              )}
            </div>
          ))}
          <div class="flex items-center gap-2 pt-1">
            <Button variant="accent" onClick={saveConfig} disabled={saving}>
              {saving ? "Saving..." : "Save"}
            </Button>
            {saved && <span class="text-xs text-vespra-green">Saved &#10003;</span>}
          </div>
        </div>
      )}
    </div>
  );
}

// ─── Main component ──────────────────────────────────────────────

export function Agents() {
  const [selected, setSelected] = useState("scout");
  const [messages, setMessages] = useState(() => {
    const loaded = {};
    for (const a of AGENTS) {
      const hist = loadHistory(a);
      if (hist.length > 0) loaded[a] = hist;
    }
    return loaded;
  });
  const [input, setInput] = useState("");
  const [sendingMap, setSendingMap] = useState({});
  const abortRef = useRef({});
  const scrollRef = useRef(null);

  const chat = messages[selected] || [];
  const sending = sendingMap[selected] || false;

  // Save to localStorage whenever messages change
  useEffect(() => {
    saveHistory(selected, chat);
  }, [selected, chat]);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [chat.length, sending]);

  const sendMessage = async (msg) => {
    if (!msg.trim() || sending) return;
    const agent = selected;
    const ts = Date.now();

    const updated = [...(messages[agent] || []), { role: "user", content: msg, ts }];
    setMessages((m) => ({ ...m, [agent]: updated }));
    setSendingMap((s) => ({ ...s, [agent]: true }));

    const controller = new AbortController();
    abortRef.current[agent] = controller;

    try {
      let enrichedMsg = msg;
      if (agent === "sentinel") {
        try {
          const wallets = await api.walletList();
          const active = (Array.isArray(wallets) ? wallets : []).filter((w) => w.active && w.chain && w.address);
          const balEntries = await Promise.all(
            active.map((w) =>
              api.balance(w.chain, w.address)
                .then((r) => `${w.label || w.wallet_id}(${w.chain}): ${r.balance_eth ?? r.balance ?? "?"} ETH`)
                .catch(() => `${w.label || w.wallet_id}(${w.chain}): ? ETH`)
            )
          );
          if (balEntries.length > 0) {
            enrichedMsg = `Wallet balances: ${balEntries.join(", ")}\n\n${msg}`;
          }
        } catch {}
      }
      const cmdText = `[${agent}] ${enrichedMsg}`;
      const res = await api.swarmCommand(cmdText, null, { signal: controller.signal });
      let content = res.reasoning || res.action_taken || res.response || JSON.stringify(res);
      // Unwrap {message|note: "..."} for every agent. The render layer
      // (AgentResponse) does this too, but normalizing at write time keeps
      // localStorage history clean and devtools inspection readable.
      try {
        const parsed = JSON.parse(content);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
          const prose = parsed.message || parsed.note;
          if (typeof prose === "string") content = prose;
        }
      } catch (_) { /* not JSON — leave content as-is */ }
      setMessages((m) => ({
        ...m,
        [agent]: [...(m[agent] || updated), { role: "agent", content, ts: Date.now() }],
      }));
    } catch (err) {
      if (err.name === "AbortError" || controller.signal.aborted) {
        setMessages((m) => ({
          ...m,
          [agent]: [...(m[agent] || updated), { role: "agent", content: "Cancelled", ts: Date.now() }],
        }));
      } else {
        setMessages((m) => ({
          ...m,
          [agent]: [
            ...(m[agent] || updated),
            { role: "agent", content: `Error: ${err.error || err.message || JSON.stringify(err)}`, ts: Date.now() },
          ],
        }));
      }
    } finally {
      delete abortRef.current[agent];
      setSendingMap((s) => ({ ...s, [agent]: false }));
    }
  };

  const cancelRequest = () => {
    const controller = abortRef.current[selected];
    if (controller) controller.abort();
  };

  const send = () => {
    if (!input.trim()) return;
    const msg = input.trim();
    setInput("");
    sendMessage(msg);
  };

  const clearChat = () => {
    setMessages((m) => ({ ...m, [selected]: [] }));
    clearHistory(selected);
  };

  const onKeyDown = (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  };

  const quickActions = QUICK_ACTIONS[selected] || [];

  return (
    <div class="space-y-4">
      <h2 class="text-xl font-bold">Agent Chat</h2>

      {/* Mobile agent selector */}
      <div class="flex gap-2 overflow-x-auto pb-2 md:hidden -mx-1 px-1">
        {AGENTS.map((a) => (
          <button
            key={a}
            onClick={() => setSelected(a)}
            class={`shrink-0 px-3 py-2.5 min-h-[44px] rounded text-sm transition-colors ${
              selected === a
                ? "bg-vespra-accent/15 text-vespra-accent border border-vespra-accent/30"
                : "text-vespra-muted bg-vespra-surface border border-vespra-border"
            }`}
          >
            <span class="capitalize">{a}</span>
            {(messages[a]?.length || 0) > 0 && (
              <span class="ml-1.5 text-xs opacity-60">{messages[a].length}</span>
            )}
          </button>
        ))}
      </div>

      <div class="flex gap-4">
        {/* Desktop agent list */}
        <div class="hidden md:block w-48 shrink-0 space-y-1">
          {AGENTS.map((a) => (
            <button
              key={a}
              onClick={() => setSelected(a)}
              class={`w-full text-left px-3 py-2.5 min-h-[44px] rounded text-sm transition-colors ${
                selected === a
                  ? "bg-vespra-accent/15 text-vespra-accent"
                  : "text-vespra-muted hover:text-vespra-text hover:bg-vespra-border/50"
              }`}
            >
              <span class="capitalize">{a}</span>
              {(messages[a]?.length || 0) > 0 && (
                <Badge variant="default" className="ml-2">
                  {messages[a].length}
                </Badge>
              )}
            </button>
          ))}
        </div>

        {/* Chat area */}
        <div class="flex-1 min-w-0">
          <Card
            title={selected}
            actions={
              chat.length > 0 && (
                <button
                  onClick={clearChat}
                  title="Clear chat and history"
                  class="px-3 py-1.5 min-h-[36px] text-vespra-muted hover:text-vespra-red hover:bg-vespra-red/10 rounded transition-colors text-xs"
                >
                  Clear
                </button>
              )
            }
          >
            {/* Agent Settings */}
            <AgentConfigPanel agent={selected} />

            {/* Archive */}
            {chat.length > 0 && (
              <ChatArchive messages={chat} agent={selected} />
            )}

            {/* Messages */}
            {chat.length > 0 && (
              <div ref={scrollRef} class="space-y-3 mb-4 max-h-[70vh] overflow-y-auto" aria-live="polite" aria-relevant="additions">
                {chat.map((msg, i) => (
                  <Message key={i} msg={msg} agent={selected} />
                ))}
                {sending && <ThinkingIndicator agent={selected} />}
              </div>
            )}

            {/* Quick actions — empty state */}
            {chat.length === 0 && quickActions.length > 0 && (
              <div class="flex flex-wrap gap-2 mb-4">
                {quickActions.map((action) => (
                  <button
                    key={action}
                    onClick={() => sendMessage(action)}
                    disabled={sending}
                    class="px-3 py-2.5 min-h-[44px] text-xs bg-vespra-border hover:bg-vespra-accent/15 hover:text-vespra-accent text-vespra-muted rounded border border-vespra-border hover:border-vespra-accent/30 transition-colors disabled:opacity-40"
                  >
                    {action}
                  </button>
                ))}
              </div>
            )}

            {/* Thinking on empty chat */}
            {chat.length === 0 && sending && (
              <div class="mb-4">
                <ThinkingIndicator agent={selected} />
              </div>
            )}

            {/* Input */}
            <div class="flex gap-2">
              <textarea
                value={input}
                onInput={(e) => setInput(e.target.value)}
                onKeyDown={onKeyDown}
                placeholder={`Message ${selected}...`}
                rows={1}
                class="flex-1 min-w-0 bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text placeholder:text-vespra-muted resize-none focus:border-vespra-accent"
              />
              {sending ? (
                <button
                  onClick={cancelRequest}
                  class="px-4 py-2.5 min-h-[44px] shrink-0 rounded text-sm font-medium transition-colors bg-vespra-red/20 hover:bg-vespra-red/30 text-vespra-red border border-vespra-red/30"
                >
                  Cancel
                </button>
              ) : (
                <Button variant="accent" onClick={send} disabled={!input.trim()} className="shrink-0">
                  Send
                </Button>
              )}
            </div>
          </Card>
        </div>
      </div>
    </div>
  );
}
