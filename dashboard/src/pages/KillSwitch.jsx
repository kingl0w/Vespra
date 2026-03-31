import { useState } from "preact/hooks";
import { useApi, usePolling } from "../hooks/useApi.js";
import { api } from "../lib/api.js";
import { Card, Button, Badge, Loader } from "../components/Card.jsx";

export function KillSwitch() {
  const [armed, setArmed] = useState(false);
  const [sweeping, setSweeping] = useState(false);
  const [results, setResults] = useState([]);
  const [killing, setKilling] = useState(false);
  const [resuming, setResuming] = useState(false);
  const [swarmMsg, setSwarmMsg] = useState(null);

  const { data: swarmStatus } = usePolling(() => api.swarmStatus().catch(() => null), 5000);
  const { data: wallets, loading } = useApi(() => api.walletList().catch(() => []), []);

  const activeWallets = (wallets || []).filter((w) => w.active);

  const killSwarm = async () => {
    setKilling(true);
    setSwarmMsg(null);
    try {
      const res = await api.swarmKill();
      setSwarmMsg({ ok: true, msg: `Swarm killed — kill_flag: ${res.kill_flag}` });
    } catch (err) {
      setSwarmMsg({ ok: false, msg: err.error || err.message || "Kill failed" });
    } finally {
      setKilling(false);
    }
  };

  const resumeSwarm = async () => {
    setResuming(true);
    setSwarmMsg(null);
    try {
      const res = await api.swarmResume();
      setSwarmMsg({ ok: true, msg: `Swarm resumed — kill_flag: ${res.kill_flag}` });
    } catch (err) {
      setSwarmMsg({ ok: false, msg: err.error || err.message || "Resume failed" });
    } finally {
      setResuming(false);
    }
  };

  const sweepAll = async () => {
    if (!armed) return;
    setSweeping(true);
    setResults([]);

    const newResults = [];
    for (const w of activeWallets) {
      try {
        const res = await api.txSweep({ wallet_id: w.id });
        if (res.status === "skip") {
          newResults.push({
            label: w.label, wallet: w.id, chain: w.chain,
            outcome: "skip", msg: res.reason || "Skipped",
          });
        } else {
          newResults.push({
            label: w.label, wallet: w.id, chain: w.chain,
            outcome: "ok", tx_hash: res.tx_hash, amount: res.amount_eth,
          });
        }
      } catch (err) {
        newResults.push({
          label: w.label, wallet: w.id, chain: w.chain,
          outcome: "error", msg: err.error || err.message || "Failed",
        });
      }
      setResults([...newResults]);
    }
    setSweeping(false);
    setArmed(false);
  };

  return (
    <div class="space-y-6">
      <h2 class="text-xl font-bold text-vespra-red">Kill Switch</h2>

      <Card title="Swarm Control" className="border-vespra-red/30">
        <div class="flex items-center justify-between py-3">
          <div class="flex items-center gap-3">
            <span class="text-sm text-vespra-muted">Swarm status:</span>
            <Badge variant={swarmStatus?.kill_flag ? "red" : "green"}>
              {swarmStatus?.kill_flag ? "KILLED" : swarmStatus ? "RUNNING" : "unknown"}
            </Badge>
            {swarmStatus?.status && (
              <span class="text-xs text-vespra-muted">{swarmStatus.status}</span>
            )}
          </div>
          <div class="flex gap-2">
            <Button variant="danger" onClick={killSwarm} disabled={killing}>
              {killing ? "Killing..." : "Kill Swarm"}
            </Button>
            <Button variant="accent" onClick={resumeSwarm} disabled={resuming}>
              {resuming ? "Resuming..." : "Resume Swarm"}
            </Button>
          </div>
        </div>
        {swarmMsg && (
          <div class={`text-sm mt-2 ${swarmMsg.ok ? "text-vespra-green" : "text-vespra-red"}`}>
            {swarmMsg.msg}
          </div>
        )}
      </Card>

      <Card className="border-vespra-red/30">
        <div class="text-center py-6 space-y-6">
          <div class="space-y-2">
            <p class="text-vespra-red font-bold text-lg">Emergency: Sweep All Wallets to Safe</p>
            <p class="text-vespra-muted text-sm max-w-md mx-auto">
              This will attempt to sweep all active burner wallets back to their configured Gnosis Safe addresses.
              Only wallets with configured Safe addresses will be swept.
            </p>
          </div>

          <div class="space-y-4">
            <div class="flex items-center justify-center gap-3">
              <label class="flex items-center gap-2 cursor-pointer select-none">
                <input
                  type="checkbox"
                  checked={armed}
                  onChange={(e) => setArmed(e.target.checked)}
                  class="w-4 h-4 accent-red-500"
                />
                <span class="text-sm text-vespra-red">Arm kill switch</span>
              </label>
            </div>

            <Button
              variant="danger"
              onClick={sweepAll}
              disabled={!armed || sweeping}
              className="px-8 py-3 text-base"
            >
              {sweeping ? "Sweeping..." : "SWEEP ALL TO SAFE"}
            </Button>
          </div>

          {loading ? (
            <Loader />
          ) : (
            <p class="text-vespra-muted text-sm">
              {activeWallets.length} active wallet{activeWallets.length !== 1 ? "s" : ""} will be swept
            </p>
          )}
        </div>
      </Card>

      {results.length > 0 && (
        <Card title="Sweep Results">
          <div class="space-y-2">
            {results.map((r, i) => (
              <div key={i} class="flex items-center justify-between py-2 border-b border-vespra-border last:border-0 text-sm gap-3">
                <div class="flex items-center gap-2 min-w-0">
                  <Badge variant={r.outcome === "ok" ? "green" : r.outcome === "skip" ? "yellow" : "red"}>
                    {r.outcome === "ok" ? "SWEPT" : r.outcome === "skip" ? "SKIP" : "FAIL"}
                  </Badge>
                  <span class="text-vespra-text font-medium truncate">{r.label || r.wallet}</span>
                  <Badge variant="accent">{r.chain}</Badge>
                </div>
                <div class="text-right text-xs shrink-0">
                  {r.outcome === "ok" && (
                    <>
                      {r.amount && <span class="text-vespra-green mr-2">{r.amount} ETH</span>}
                      <span class="font-mono text-vespra-muted">tx {r.tx_hash?.slice(0, 10)}...{r.tx_hash?.slice(-4)}</span>
                    </>
                  )}
                  {r.outcome === "skip" && (
                    <span class="text-vespra-yellow">{r.msg}</span>
                  )}
                  {r.outcome === "error" && (
                    <span class="text-vespra-red">{r.msg}</span>
                  )}
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}

      {activeWallets.length > 0 && (
        <Card title="Active Wallets">
          <div class="space-y-1">
            {activeWallets.map((w) => (
              <div key={w.id} class="flex items-center justify-between py-1.5 text-sm">
                <div class="flex items-center gap-2">
                  <span class="text-vespra-text font-medium">{w.label || w.id}</span>
                  <Badge variant="accent">{w.chain}</Badge>
                </div>
                <span class="font-mono text-xs text-vespra-muted">
                  {w.address?.slice(0, 8)}...{w.address?.slice(-4)}
                </span>
              </div>
            ))}
          </div>
        </Card>
      )}
    </div>
  );
}
