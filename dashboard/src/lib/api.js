const BASE = import.meta.env.MODE === "production"
  ? "https://api.vespra.xyz/api"
  : "/api";

const BASE_GW = import.meta.env.MODE === "production"
  ? "https://api.vespra.xyz"
  : "http://127.0.0.1:9001";

async function request(path, opts = {}) {
  const url = `${BASE}${path}`;
  const res = await fetch(url, {
    headers: { "Content-Type": "application/json", ...opts.headers },
    ...opts,
  });
  const text = await res.text();
  let data;
  try {
    data = JSON.parse(text);
  } catch {
    if (!res.ok) throw { status: res.status, error: text || `HTTP ${res.status}` };
    throw { status: res.status, error: `Invalid JSON response: ${text.slice(0, 200)}` };
  }
  if (!res.ok) throw { status: res.status, ...data };
  return data;
}

export const api = {
  // Health
  health: () => request("/health"),
  healthAll: () => request("/health"),
  // Returns { gateway: {...}, boiler: {...}, keymaster: {...} }

  // Swarm
  swarmCommand: (command, walletId, { signal } = {}) =>
    fetch(`${BASE_GW}/swarm/command`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ command, ...(walletId ? { wallet_id: walletId } : {}) }),
      signal,
    }).then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  swarmKill: () =>
    fetch(`${BASE_GW}/swarm/kill`, { method: "POST", headers: { "Content-Type": "application/json" } })
      .then(r => r.json()),

  swarmResume: () =>
    fetch(`${BASE_GW}/swarm/resume`, { method: "POST", headers: { "Content-Type": "application/json" } })
      .then(r => r.json()),

  swarmStatus: () =>
    fetch(`${BASE_GW}/swarm/status`)
      .then(r => r.json()),

  // DAGs
  dagList: () => request("/dag"),
  dagSubmit: (dag) =>
    request("/dag", { method: "POST", body: JSON.stringify(dag) }),
  dagGet: (id) => request(`/dag/${id}`),

  // Wallets
  walletList: () => request("/wallet"),
  walletGet: (id) => request(`/wallet/${id}`),
  walletCreate: (body) =>
    request("/dispatch", {
      method: "POST",
      body: JSON.stringify({
        task_id: `dash-${Date.now()}`,
        prompt: "create_wallet",
        input: { action: "create_wallet", ...body },
      }),
    }),

  // Balances
  balance: (chain, address) => request(`/balance/${chain}/${address}`),
  balancesAll: (chain) => request(`/balances/${chain}`),

  // Chain
  chainStatus: (chain) => request(`/chain/${chain}`),

  // TX
  txLog: (walletId) => request(`/tx/log/${walletId}`),
  txSend: (body) =>
    request("/dispatch", {
      method: "POST",
      body: JSON.stringify({
        task_id: `dash-${Date.now()}`,
        prompt: "send_native",
        input: { action: "send_native", ...body },
      }),
    }),
  txSweep: (body) =>
    request("/tx/sweep", {
      method: "POST",
      body: JSON.stringify(body),
    }),

  // Settings
  safesGet: () => request("/settings/safes"),
  safeSet: (chain, address) =>
    request(`/settings/safes/${chain}`, {
      method: "PUT",
      body: JSON.stringify({ address }),
    }),

  // Dispatch (generic)
  dispatch: (action, params) =>
    request("/dispatch", {
      method: "POST",
      body: JSON.stringify({
        task_id: `dash-${Date.now()}`,
        prompt: action,
        input: { action, ...params },
      }),
    }),
};
