import { useState, useEffect, useCallback, useRef } from "preact/hooks";
import { useApi, usePolling } from "../hooks/useApi.js";
import { useChain } from "../hooks/useChain.jsx";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

const CHAINS = ["sepolia", "base_sepolia", "arbitrum_sepolia", "ethereum", "base", "arbitrum", "optimism"];

const CHAIN_LABELS = {
  sepolia: "Sepolia",
  base_sepolia: "Base Sepolia",
  arbitrum_sepolia: "Arbitrum Sepolia",
  ethereum: "Ethereum",
  base: "Base",
  arbitrum: "Arbitrum",
  optimism: "Optimism",
};

function chainLabel(id) {
  return CHAIN_LABELS[id] || id;
}

function safeEth(value) {
  const n = parseFloat(value);
  return Number.isFinite(n) ? parseFloat(n.toFixed(4)) : null;
}

function CopyButton({ text }) {
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  const copy = async () => {
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        // Fallback for insecure contexts
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.cssText = "position:fixed;left:-9999px";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      setFailed(true);
      setTimeout(() => setFailed(false), 2000);
    }
  };
  return (
    <button
      onClick={copy}
      aria-label={copied ? "Copied to clipboard" : failed ? "Copy failed" : "Copy to clipboard"}
      class="px-3 py-1.5 min-h-[36px] text-xs bg-vespra-border hover:bg-vespra-accent/15 hover:text-vespra-accent text-vespra-muted rounded transition-colors"
    >
      {copied ? "Copied" : failed ? "Failed" : "Copy"}
    </button>
  );
}

function BalanceSpinner() {
  return (
    <span class="inline-flex items-center" role="status">
      <span class="inline-block w-3 h-3 border border-vespra-accent border-t-transparent rounded-full animate-spin" />
      <span class="sr-only">Loading balance...</span>
    </span>
  );
}

function useWalletBalances() {
  const [balances, setBalances] = useState({});
  const [loading, setLoading] = useState({});
  const hasFetched = useState({ current: false })[0];

  const fetchBalances = useCallback((wallets) => {
    if (!wallets || !Array.isArray(wallets)) return;
    const active = wallets.filter((w) => w.active && w.chain && w.address);
    // Throttle: fetch 3 at a time to avoid API overload
    let i = 0;
    const next = () => {
      if (i >= active.length) return;
      const w = active[i++];
      const key = w.id || w.wallet_id;
      setLoading((prev) => ({ ...prev, [key]: true }));
      api.balance(w.chain, w.address)
        .then((res) => {
          setBalances((prev) => ({ ...prev, [key]: res.balance_eth ?? res.balance ?? null }));
        })
        .catch(() => {
          setBalances((prev) => ({ ...prev, [key]: null }));
        })
        .finally(() => {
          setLoading((prev) => ({ ...prev, [key]: false }));
          next();
        });
    };
    // Start up to 3 concurrent fetches
    const concurrency = Math.min(3, active.length);
    for (let c = 0; c < concurrency; c++) next();
  }, []);

  const fetchOnce = useCallback((wallets) => {
    if (hasFetched.current) return;
    hasFetched.current = true;
    fetchBalances(wallets);
  }, [fetchBalances, hasFetched]);

  const setBalance = useCallback((key, value) => {
    setBalances((prev) => ({ ...prev, [key]: value }));
  }, []);

  return { balances, loading, fetchBalances, fetchOnce, setBalance };
}

function WalletRow({ wallet, onSelect, balance, balanceLoading }) {
  const handleKeyDown = (e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onSelect(wallet);
    }
  };
  return (
    <tr
      class="border-b border-vespra-border hover:bg-vespra-border/30 cursor-pointer transition-colors"
      onClick={() => onSelect(wallet)}
      onKeyDown={handleKeyDown}
      tabIndex={0}
      role="button"
      aria-label={`View wallet ${wallet.label || wallet.wallet_id}`}
    >
      <td class="py-2.5 px-3">
        <div class="text-sm font-medium">{wallet.label || wallet.wallet_id}</div>
        {wallet.label && (
          <div class="text-xs text-vespra-muted font-mono">{wallet.wallet_id}</div>
        )}
      </td>
      <td class="py-2.5 px-3">
        <Badge variant="accent">{chainLabel(wallet.chain)}</Badge>
      </td>
      <td class="py-2.5 px-3 font-mono text-sm">
        <span class="text-vespra-muted">{wallet.address?.slice(0, 6)}...{wallet.address?.slice(-4)}</span>
      </td>
      <td class="py-2.5 px-3 text-sm text-right font-mono">
        {!wallet.active ? (
          <span class="text-vespra-muted">-</span>
        ) : balanceLoading ? (
          <BalanceSpinner />
        ) : balance != null ? (
          <span>{safeEth(balance)} <span class="text-vespra-muted text-xs">ETH</span></span>
        ) : (
          <span class="text-vespra-muted">-</span>
        )}
      </td>
      <td class="py-2.5 px-3">
        <Badge variant={wallet.active ? "green" : "red"}>
          {wallet.active ? "active" : "inactive"}
        </Badge>
      </td>
      <td class="py-2.5 px-3 text-sm text-vespra-muted">
        {wallet.strategy || "-"}
      </td>
    </tr>
  );
}

function NewWalletCard({ wallet, onDismiss }) {
  const [checking, setChecking] = useState(false);
  const [fundStatus, setFundStatus] = useState(null);
  const dismissTimer = useRef(null);

  useEffect(() => () => { clearTimeout(dismissTimer.current); }, []);

  const checkBalance = async () => {
    setChecking(true);
    setFundStatus(null);
    try {
      const res = await api.balance(wallet.chain, wallet.address);
      const bal = parseFloat(res.balance_eth || res.balance || "0");
      if (bal > 0) {
        setFundStatus({ funded: true, amount: safeEth(bal) });
        dismissTimer.current = setTimeout(onDismiss, 3000);
      } else {
        setFundStatus({ funded: false });
      }
    } catch {
      setFundStatus({ funded: false });
    } finally {
      setChecking(false);
    }
  };

  return (
    <Card className="border-vespra-accent/30" title="Wallet Created">
      <div class="space-y-4">
        <div class="text-center py-2">
          <p class="text-vespra-accent text-sm font-medium mb-2">Fund this wallet from your Safe or MetaMask</p>
          <div class="bg-vespra-bg border border-vespra-border rounded-lg p-4 inline-block">
            <div class="font-mono text-lg text-vespra-text tracking-wider">{wallet.address}</div>
            <div class="flex items-center justify-center gap-3 mt-3">
              <Badge variant="accent">{chainLabel(wallet.chain)}</Badge>
              <CopyButton text={wallet.address} />
            </div>
            <div class="mt-4">
              <Button variant="ghost" onClick={checkBalance} disabled={checking}>
                {checking ? "Checking..." : "I've sent funds — check balance"}
              </Button>
              {fundStatus && (
                <p class={`text-sm mt-2 ${fundStatus.funded ? "text-vespra-green" : "text-vespra-muted"}`}>
                  {fundStatus.funded
                    ? `\u2713 ${fundStatus.amount} ETH received!`
                    : "No funds received yet — try again in a moment."}
                </p>
              )}
            </div>
          </div>
        </div>
        <div class="text-center">
          <p class="text-vespra-muted text-xs">
            Wallet ID: <span class="font-mono">{wallet.wallet_id}</span>
          </p>
          <Button variant="ghost" onClick={onDismiss} className="mt-3">
            Dismiss
          </Button>
        </div>
      </div>
    </Card>
  );
}

function CreateWalletForm({ onCreated }) {
  const [chain, setChain] = useState("sepolia");
  const [label, setLabel] = useState("");
  const [creating, setCreating] = useState(false);
  const [result, setResult] = useState(null);
  const [newWallet, setNewWallet] = useState(null);

  const create = async () => {
    setCreating(true);
    setResult(null);
    setNewWallet(null);
    try {
      const res = await api.walletCreate({ chain, label: label || undefined });
      const resp = res.response || res;
      setNewWallet({
        address: resp.address || resp.wallet_address || "Unknown",
        wallet_id: resp.wallet_id || "Unknown",
        chain,
      });
      setResult({ ok: true });
      setLabel("");
      onCreated?.();
    } catch (err) {
      setResult({ ok: false, msg: err.error || err.message || "Wallet creation failed" });
    } finally {
      setCreating(false);
    }
  };

  return (
    <div class="space-y-4">
      <div class="flex flex-col sm:flex-row items-stretch sm:items-end gap-3">
        <div>
          <label for="wallet-chain" class="text-xs text-vespra-muted block mb-1">Chain</label>
          <select
            id="wallet-chain"
            value={chain}
            onChange={(e) => setChain(e.target.value)}
            class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text focus:border-vespra-accent focus:outline-none w-full"
          >
            {CHAINS.map((c) => (
              <option key={c} value={c}>{chainLabel(c)}</option>
            ))}
          </select>
        </div>
        <div>
          <label for="wallet-label" class="text-xs text-vespra-muted block mb-1">Label</label>
          <input
            id="wallet-label"
            value={label}
            onInput={(e) => setLabel(e.target.value)}
            placeholder="optional"
            maxLength={64}
            class="bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm text-vespra-text placeholder:text-vespra-muted focus:border-vespra-accent focus:outline-none w-full sm:w-40"
          />
        </div>
        <Button variant="accent" onClick={create} disabled={creating}>
          {creating ? "Creating..." : "Create Wallet"}
        </Button>
        {result && !result.ok && (
          <span class="text-sm text-vespra-red" role="alert">{result.msg}</span>
        )}
      </div>
      {newWallet && (
        <NewWalletCard wallet={newWallet} onDismiss={() => setNewWallet(null)} />
      )}
    </div>
  );
}

function WalletDetail({ wallet, onClose, onBalanceUpdate }) {
  const { data: balance, loading: balLoading, refresh: refreshBalance } = useApi(
    () => api.balance(wallet.chain, wallet.address).catch(() => null),
    [wallet.id, wallet.wallet_id]
  );
  const refreshRef = useRef(refreshBalance);
  refreshRef.current = refreshBalance;
  const [sweeping, setSweeping] = useState(false);
  const [sweepResult, setSweepResult] = useState(null);

  // Propagate balance to table whenever it updates
  const prevBalRef = useRef(null);
  useEffect(() => {
    if (!balance) return;
    const eth = balance.balance_eth ?? balance.balance ?? null;
    if (eth !== null && eth !== prevBalRef.current) {
      prevBalRef.current = eth;
      const key = wallet.id || wallet.wallet_id;
      onBalanceUpdate?.(key, eth);
    }
  }, [balance, wallet.id, wallet.wallet_id, onBalanceUpdate]);

  const sweep = async () => {
    setSweeping(true);
    setSweepResult(null);
    try {
      const res = await api.txSweep({ wallet_id: wallet.id || wallet.wallet_id });
      const resp = res.response || res;
      setSweepResult({ ok: true, msg: resp.status === "skip" ? (resp.reason || "Skipped") : resp.tx_hash ? `Swept — tx ${resp.tx_hash.slice(0, 14)}...` : "Sweep submitted" });
      setTimeout(() => refreshRef.current(), 3000);
    } catch (err) {
      setSweepResult({ ok: false, msg: err.error || err.message || "Sweep failed" });
    } finally {
      setSweeping(false);
    }
  };

  const balEth = balance ? safeEth(balance.balance_eth ?? balance.balance) : null;

  return (
    <Card
      title={wallet.label || wallet.wallet_id}
      actions={<Button variant="ghost" onClick={onClose}>Close</Button>}
    >
      <div class="grid grid-cols-2 gap-4 text-sm">
        <div>
          <span class="text-vespra-muted">Wallet ID</span>
          <div class="font-mono mt-1">{wallet.wallet_id || wallet.id}</div>
        </div>
        <div>
          <span class="text-vespra-muted">Chain</span>
          <div class="mt-1"><Badge variant="accent">{chainLabel(wallet.chain)}</Badge></div>
        </div>
        <div class="col-span-2">
          <span class="text-vespra-muted">Address</span>
          <div class="font-mono mt-1 flex items-center gap-2">
            <span class="text-vespra-accent break-all">{wallet.address}</span>
            <CopyButton text={wallet.address} />
          </div>
        </div>
        <div>
          <span class="text-vespra-muted">Balance</span>
          <div class="mt-1 flex items-center gap-1.5">
            <span class="text-lg font-bold">
              {balLoading ? "..." : balEth != null ? balEth : "-"}
              <span class="text-vespra-muted text-sm ml-1">ETH</span>
            </span>
            <button
              onClick={refreshBalance}
              disabled={balLoading}
              class="text-vespra-muted hover:text-vespra-accent transition-colors disabled:opacity-40 p-2 -m-1.5 rounded"
              aria-label="Refresh balance"
            >
              <svg class={`w-3.5 h-3.5 ${balLoading ? "animate-spin" : ""}`} viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2">
                <path d="M14 8A6 6 0 1 1 10 2.5" stroke-linecap="round" />
                <path d="M14 2v4h-4" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
            </button>
          </div>
        </div>
        <div>
          <span class="text-vespra-muted">Cap</span>
          <div class="mt-1">{wallet.cap_eth ? `${wallet.cap_eth} ETH` : "None"}</div>
        </div>
        <div class="col-span-2 pt-2 border-t border-vespra-border flex items-center gap-3">
          <Button variant="accent" onClick={sweep} disabled={sweeping || !wallet.active}>
            {sweeping ? "Sweeping..." : "Sweep to Safe"}
          </Button>
          {sweepResult && (
            <span class={`text-sm ${sweepResult.ok ? "text-vespra-green" : "text-vespra-red"}`} role="status">
              {sweepResult.msg}
            </span>
          )}
        </div>
      </div>
    </Card>
  );
}

export function Wallets() {
  const [selected, setSelected] = useState(null);
  const { chain: globalChain } = useChain();
  const { data: wallets, loading, refresh } = usePolling(
    () => api.walletList().catch(() => []),
    15000
  );

  const filtered = wallets && Array.isArray(wallets)
    ? globalChain === "all"
      ? wallets
      : wallets.filter((w) => w.chain === globalChain)
    : [];

  const { balances, loading: balLoading, fetchBalances, fetchOnce, setBalance: updateTableBalance } = useWalletBalances();

  useEffect(() => {
    if (wallets) fetchOnce(wallets);
  }, [wallets, fetchOnce]);

  const refreshAll = () => {
    refresh();
    if (wallets) fetchBalances(wallets);
  };

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Wallet Dashboard</h2>

      {selected && (
        <WalletDetail key={selected.id || selected.wallet_id} wallet={selected} onClose={() => setSelected(null)} onBalanceUpdate={updateTableBalance} />
      )}

      <Card title="Create Wallet">
        <CreateWalletForm onCreated={refreshAll} />
      </Card>

      <Card
        title={`Wallets (${filtered.length})${globalChain !== "all" ? ` on ${globalChain}` : ""}`}
        actions={<Button variant="ghost" onClick={refreshAll}>Refresh</Button>}
      >
        {loading && !wallets ? (
          <Loader />
        ) : filtered.length === 0 ? (
          <p class="text-vespra-muted text-sm">No wallets found{globalChain !== "all" ? ` on ${globalChain}` : ""}</p>
        ) : (
          <div class="overflow-x-auto">
            <table class="w-full">
              <thead>
                <tr class="text-left text-xs text-vespra-muted border-b border-vespra-border">
                  <th scope="col" class="py-2 px-3 font-medium">Wallet</th>
                  <th scope="col" class="py-2 px-3 font-medium">Chain</th>
                  <th scope="col" class="py-2 px-3 font-medium">Address</th>
                  <th scope="col" class="py-2 px-3 font-medium text-right">Balance</th>
                  <th scope="col" class="py-2 px-3 font-medium">Status</th>
                  <th scope="col" class="py-2 px-3 font-medium">Strategy</th>
                </tr>
              </thead>
              <tbody>
                {filtered.map((w) => {
                  const key = w.id || w.wallet_id;
                  return (
                    <WalletRow
                      key={key}
                      wallet={w}
                      onSelect={setSelected}
                      balance={balances[key]}
                      balanceLoading={balLoading[key]}
                    />
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
