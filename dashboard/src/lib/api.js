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

  // Trade Up (VES-37)
  tradeUpStart: (wallet, chain) =>
    fetch(`${BASE_GW}/trade-up/position/start`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ wallet, chain }),
    }).then(r => r.json()),

  tradeUpStop: () =>
    fetch(`${BASE_GW}/trade-up/position/stop`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
    }).then(r => r.json()),

  tradeUpStatus: () =>
    fetch(`${BASE_GW}/trade-up/position/status`)
      .then(r => r.json()),

  tradeUpHistory: () =>
    fetch(`${BASE_GW}/trade-up/position/history`)
      .then(r => r.json()),

  // Coordinator
  orchestrate: (intent, wallet, chain) =>
    fetch(`${BASE_GW}/coordinator/orchestrate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ intent, ...(wallet ? { wallet } : {}), ...(chain ? { chain } : {}) }),
    }).then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  getSessionContext: () =>
    fetch(`${BASE_GW}/coordinator/session`)
      .then(r => r.json()),

  // Goals (VES-87)
  fetchGoals: () =>
    fetch(`${BASE_GW}/goals`)
      .then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  fetchGoal: (id) =>
    fetch(`${BASE_GW}/goals/${id}`)
      .then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  createGoal: (payload) =>
    fetch(`${BASE_GW}/goals`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }).then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  cancelGoal: (id) =>
    fetch(`${BASE_GW}/goals/${id}/cancel`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
    }).then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  pauseGoal: (id) =>
    fetch(`${BASE_GW}/goals/${id}/pause`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
    }).then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  resumeGoal: (id) =>
    fetch(`${BASE_GW}/goals/${id}/resume`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
    }).then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

  fetchPortfolio: () =>
    fetch(`${BASE_GW}/goals/portfolio`)
      .then(r => r.ok ? r.json() : r.json().then(e => Promise.reject(e))),

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
