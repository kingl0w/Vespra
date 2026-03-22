import { useState } from "preact/hooks";
import { useApi, usePolling } from "../hooks/useApi.js";
import { useChain } from "../hooks/useChain.jsx";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

function TxRow({ tx }) {
  return (
    <tr class="border-b border-vespra-border text-sm">
      <td class="py-2.5 px-3 text-vespra-muted font-mono text-xs">
        {tx.timestamp ? new Date(tx.timestamp).toLocaleString() : "-"}
      </td>
      <td class="py-2.5 px-3">
        <Badge variant="accent">{tx.chain}</Badge>
      </td>
      <td class="py-2.5 px-3 font-mono text-xs">
        {tx.wallet_id}
      </td>
      <td class="py-2.5 px-3 font-mono text-xs text-vespra-muted">
        {tx.to_address ? `${tx.to_address.slice(0, 8)}...${tx.to_address.slice(-4)}` : "-"}
      </td>
      <td class="py-2.5 px-3 text-right">
        {tx.amount_eth || tx.value || "-"}
      </td>
      <td class="py-2.5 px-3 font-mono text-xs">
        {tx.tx_hash ? (
          <span class="text-vespra-accent">
            {tx.tx_hash.slice(0, 10)}...{tx.tx_hash.slice(-4)}
          </span>
        ) : "-"}
      </td>
      <td class="py-2.5 px-3">
        <Badge variant={tx.status === "confirmed" ? "green" : tx.status === "failed" ? "red" : "yellow"}>
          {tx.status || "unknown"}
        </Badge>
      </td>
    </tr>
  );
}

export function TxLog() {
  const [walletId, setWalletId] = useState("");
  const [searchId, setSearchId] = useState("");
  const { chain: globalChain } = useChain();

  const { data: wallets } = useApi(() => api.walletList().catch(() => []), []);
  const { data: txs, loading, refresh } = usePolling(
    () => searchId ? api.txLog(searchId).catch(() => []) : Promise.resolve([]),
    10000,
    [searchId]
  );

  const filteredTxs = txs && Array.isArray(txs) && globalChain !== "all"
    ? txs.filter((tx) => tx.chain === globalChain)
    : txs;

  const filteredWallets = wallets && Array.isArray(wallets) && globalChain !== "all"
    ? wallets.filter((w) => w.chain === globalChain)
    : wallets;

  const search = () => {
    if (walletId.trim()) setSearchId(walletId.trim());
  };

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Transaction Log</h2>

      <Card title="Search">
        <div class="flex gap-3 items-end">
          <div class="flex-1">
            <label class="text-xs text-vespra-muted block mb-1">Wallet ID</label>
            <div class="flex gap-2">
              <input
                value={walletId}
                onInput={(e) => setWalletId(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && search()}
                placeholder="Enter wallet ID or select below"
                class="flex-1 bg-vespra-bg border border-vespra-border rounded px-3 py-1.5 text-sm text-vespra-text placeholder:text-vespra-muted focus:outline-none focus:border-vespra-accent"
              />
              <Button variant="accent" onClick={search}>Search</Button>
            </div>
          </div>
        </div>
        {filteredWallets && Array.isArray(filteredWallets) && filteredWallets.length > 0 && (
          <div class="flex flex-wrap gap-1.5 mt-3">
            {filteredWallets.map((w) => (
              <button
                key={w.wallet_id}
                onClick={() => { setWalletId(w.wallet_id); setSearchId(w.wallet_id); }}
                class={`px-2 py-1 rounded text-xs transition-colors ${
                  searchId === w.wallet_id
                    ? "bg-vespra-accent/15 text-vespra-accent"
                    : "bg-vespra-border text-vespra-muted hover:text-vespra-text"
                }`}
              >
                {w.label || w.wallet_id}
              </button>
            ))}
          </div>
        )}
      </Card>

      {searchId && (
        <Card
          title={`Transactions for ${searchId}${globalChain !== "all" ? ` (${globalChain})` : ""}`}
          actions={<Button variant="ghost" onClick={refresh}>Refresh</Button>}
        >
          {loading && !txs ? (
            <Loader />
          ) : !filteredTxs || !Array.isArray(filteredTxs) || filteredTxs.length === 0 ? (
            <p class="text-vespra-muted text-sm">No transactions found</p>
          ) : (
            <div class="overflow-x-auto">
              <table class="w-full">
                <thead>
                  <tr class="text-left text-xs text-vespra-muted border-b border-vespra-border">
                    <th class="py-2 px-3 font-medium">Time</th>
                    <th class="py-2 px-3 font-medium">Chain</th>
                    <th class="py-2 px-3 font-medium">Wallet</th>
                    <th class="py-2 px-3 font-medium">To</th>
                    <th class="py-2 px-3 font-medium text-right">Amount</th>
                    <th class="py-2 px-3 font-medium">TX Hash</th>
                    <th class="py-2 px-3 font-medium">Status</th>
                  </tr>
                </thead>
                <tbody>
                  {filteredTxs.map((tx, i) => <TxRow key={i} tx={tx} />)}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
