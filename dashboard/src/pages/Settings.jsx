import { useState, useEffect } from "preact/hooks";
import { usePolling } from "../hooks/useApi.js";
import { api } from "../lib/api.js";
import { Card, Button, Badge, StatusDot, Loader } from "../components/Card.jsx";

const CHAIN_IDS = ["ethereum", "base", "arbitrum", "optimism", "sepolia", "base_sepolia", "arbitrum_sepolia"];

export function Settings() {
  const [safes, setSafes] = useState({});
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState(null); // "saved" | "error"
  const { data: health, loading } = usePolling(() => api.health(), 15000);

  // Load safes from API on mount
  useEffect(() => {
    api.safesGet()
      .then((data) => setSafes(data))
      .catch(() => {}); // API may not be up yet
  }, []);

  const updateSafe = (chain, value) => {
    setSafes((s) => ({ ...s, [chain]: value }));
    setStatus(null);
  };

  const save = async () => {
    setSaving(true);
    setStatus(null);
    try {
      const entries = Object.entries(safes).filter(([, v]) => v && v.trim());
      for (const [chain, address] of entries) {
        await api.safeSet(chain, address.trim());
      }
      setStatus("saved");
      setTimeout(() => setStatus(null), 2000);
    } catch {
      setStatus("error");
    } finally {
      setSaving(false);
    }
  };

  const services = health?.services || {};

  const [chainStatuses, setChainStatuses] = useState({});
  useEffect(() => {
    CHAIN_IDS.forEach((chain) => {
      api.chainStatus(chain)
        .then((data) => setChainStatuses((s) => ({ ...s, [chain]: { ok: true, data } })))
        .catch(() => setChainStatuses((s) => ({ ...s, [chain]: { ok: false } })));
    });
  }, []);

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold">Settings</h2>

      <Card title="Connection Status">
        {loading && !health ? (
          <Loader />
        ) : (
          <div class="space-y-3">
            {Object.entries(services).map(([name, svc]) => (
              <div key={name} class="flex items-center justify-between py-2 border-b border-vespra-border last:border-0">
                <div class="flex items-center gap-3">
                  <StatusDot status={svc.status} />
                  <span class="text-sm font-medium capitalize">{name}</span>
                </div>
                <Badge variant={svc.status === "ok" ? "green" : "red"}>{svc.status}</Badge>
              </div>
            ))}
          </div>
        )}
      </Card>

      <Card title="Chain RPC Status">
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
          {CHAIN_IDS.map((chain) => {
            const cs = chainStatuses[chain];
            return (
              <div key={chain} class="flex items-center gap-2 py-2 px-3 bg-vespra-bg rounded border border-vespra-border">
                <StatusDot status={cs?.ok ? "ok" : cs ? "down" : "unknown"} />
                <span class="text-sm">{chain}</span>
              </div>
            );
          })}
        </div>
      </Card>

      <Card
        title="Gnosis Safe Addresses"
        actions={
          <div class="flex items-center gap-2">
            {status === "saved" && <span class="text-vespra-green text-xs">Saved</span>}
            {status === "error" && <span class="text-vespra-red text-xs">Save failed</span>}
            <Button variant="accent" onClick={save} disabled={saving}>
              {saving ? "Saving..." : "Save"}
            </Button>
          </div>
        }
      >
        <p class="text-vespra-muted text-xs mb-4">
          Safe addresses are stored in Keymaster's database. Used for sweep/kill-switch operations.
        </p>
        <div class="space-y-3">
          {CHAIN_IDS.map((chain) => (
            <div key={chain} class="flex flex-col sm:flex-row sm:items-center gap-1 sm:gap-3">
              <label class="text-sm text-vespra-muted sm:w-36 shrink-0">{chain}</label>
              <input
                value={safes[chain] || ""}
                onInput={(e) => updateSafe(chain, e.target.value)}
                placeholder="0x..."
                class="flex-1 bg-vespra-bg border border-vespra-border rounded px-3 py-2.5 min-h-[44px] text-sm font-mono text-vespra-text placeholder:text-vespra-muted/40 focus:border-vespra-accent"
              />
            </div>
          ))}
        </div>
      </Card>
    </div>
  );
}
