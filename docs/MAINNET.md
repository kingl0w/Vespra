# Mainnet Transition Guide

You're about to hook Vespra up to real funds on real chains. Read this file end-to-end
before you change a single config value.

> **This is beta software that has not been externally audited.** Treat every mainnet
> dollar as potentially at risk. The recommendations below are the minimum bar, not a
> guarantee.

---

## 1. Pre-Flight Checklist

Don't proceed until every one of these is true:

- [ ] Stack has been running on testnet continuously for **at least a week** — not just
      "a few hours yesterday." You're trying to catch the crash-loop that only happens
      on day 4 when the Redis TTL sweeper runs for the first time.
- [ ] **At least 5 testnet goals completed end-to-end** — from SCOUTING through
      EXECUTING to a final EXIT state. Not "it got to MONITORING once." A full round
      trip exercises every agent.
- [ ] **Telegram notifications work.** Trigger a goal, confirm messages land in the
      chat you configured. If they don't, fix that before moving to mainnet — you need
      the pager.
- [ ] **Kill switch tested end-to-end.** Run this sequence on testnet:
  ```bash
  # activate
  curl -X POST http://localhost:9001/swarm/kill
  # confirm keymaster rejects signing
  curl -X POST http://localhost:9100/tx/send \
    -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN" \
    -H 'Content-Type: application/json' \
    -d '{"wallet_id":"<id>","to":"0x0...1","amount_eth":"0.001"}'
  # expect: 503 kill switch active — signing disabled
  # deactivate
  curl -X POST http://localhost:9001/swarm/resume
  ```
  If you can't reproduce that 503, stop — something is misconfigured.
- [ ] `./scripts/doctor.sh` returns all green with no ❌.
- [ ] You have a Telegram-accessible device within arm's reach for the first 48 hours.
- [ ] You've backed up `.env` **and** the Keymaster data volume. Losing
      `KEYMASTER_MASTER_PASSWORD` means losing every wallet. Losing
      `/opt/vespra-keymaster/keymaster.db` means losing every wallet.

---

## 2. RPC Provider Recommendations

Public RPCs work for testnets. **Do not run mainnet on a public RPC.** They rate-limit,
lie about quotes under load, and have no SLA.

| Provider | Free tier | Paid tier starts at | Notes |
|---|---|---|---|
| [Alchemy](https://alchemy.com) | 300M compute units/month | $49/mo (Growth) | Best docs; enhanced APIs the gateway doesn't currently use but reduces round-trips. Strong multi-chain coverage |
| [Infura](https://infura.io) | 100k requests/day | $50/mo (Developer) | ConsenSys-backed; reliable but quota burns fast under Sentinel's 5-minute sweep |
| [QuickNode](https://quicknode.com) | 10M API credits/month trial | $9/mo (Starter) | Cheapest paid tier; per-endpoint pricing, so multi-chain adds up |
| **Public endpoints** | *(free)* | — | **Discouraged.** No SLA, aggressive rate limits, frequent lies about `eth_call` under load. Fine for read-only testnet work; not for mainnet signing |

**Recommended baseline:** a paid tier with **at least 300 req/s headroom** above your
expected steady-state. The Sentinel monitor alone polls every open position every
5 minutes, and yield rotation scans every 30 minutes — both burn compute units fast if
you're running more than a handful of goals.

Check your actual usage after a week with the provider's dashboard; scale up if you're
seeing throttling in gateway logs (`eth_getBalance failed`, `429`, or `timeout` lines).

---

## 3. Step-by-Step Transition

### 3.1 Generate a fresh mainnet burner wallet

**Do not reuse a testnet wallet.** Testnet and mainnet addresses are the same space,
but the private key may have been written to testnet logs, faucet databases, or random
tooling you forgot about. Create a new one.

```bash
source .env
curl -X POST http://localhost:9100/wallets \
  -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"chain":"base","label":"mainnet-01","cap_eth":"0.05"}'
```

Copy the returned `address`. The `cap_eth` is Keymaster's per-tx spend ceiling — start
tight (0.05 or less) and raise it later.

### 3.2 Fund it with a small amount

**0.01–0.05 ETH for your first run.** Not "whatever I had lying around." You need to
see a full goal complete before you trust the pipeline with more.

Send from your cold wallet / hardware wallet to the burner address. Confirm the tx
landed.

### 3.3 Update `.env`

```env
# swap the RPC URLs
RPC_URL_BASE=https://base-mainnet.g.alchemy.com/v2/<your-key>
RPC_URL_ARBITRUM=https://arb-mainnet.g.alchemy.com/v2/<your-key>

# flip the network mode
VESPRA_NETWORK_MODE=mainnet

# (recommended) set a global cap so an LLM hallucinating "10 ETH" can't trip
VESPRA_MAX_GLOBAL_WALLET_VALUE_ETH=0.1
```

### 3.4 (Optional) Enable fees

Only if you're running for others. Skip this for personal use.

```env
FEES_ENABLED=true
TREASURY_ADDRESS=0xYourTreasury
```

Keymaster validates `TREASURY_ADDRESS` is a proper checksum address at boot. It will
refuse to start otherwise.

### 3.5 Restart the stack

```bash
make restart
```

### 3.6 Verify boot logs show mainnet mode

```bash
docker compose logs gateway | grep '\[network\]'
```

You should see:

```
[network] mode=mainnet — strict risk gates, real pool data required
```

If you see `mode=testnet`, your `.env` change didn't propagate — check that
`make restart` didn't re-use an old image, and that `.env` is at the repo root.

### 3.7 Submit your first goal with the smallest possible amount

```bash
curl -X POST http://localhost:9001/goals \
  -H 'Content-Type: application/json' \
  -d '{
    "raw_goal":"Compound 0.001 ETH on Base, exit at 3% gain or -2% loss",
    "wallet_label":"mainnet-01"
  }'
```

### 3.8 Watch the pipeline

Open three terminals:

```bash
# 1. gateway logs
docker compose logs -f gateway

# 2. keymaster logs
docker compose logs -f keymaster

# 3. doctor status every minute
watch -n 60 './scripts/doctor.sh'
```

And keep Telegram open. You should see, in rough order:
1. `[goal <id>] SCOUTING` → pool selected
2. `[goal <id>] RISK` → LOW risk passes on mainnet
3. `[goal <id>] TRADING` → trader decides SWAP
4. Keymaster logs the `/swap` POST + tx hash
5. `[goal <id>] BUY confirmed, tx=0x...`
6. `[goal <id>] MONITORING`
7. Telegram message for goal entry

If you see `[risk] testnet mode` on mainnet — stop, your mode didn't flip. If you see
`injecting synthetic WETH/USDC fallback` — stop, you're still on testnet mode.

---

## 4. First 30 Days Operational Guide

The goal here is to build confidence before scaling up capital or strategy complexity.

### 4.1 Daily

- **Check Telegram every morning.** Any `goal failed` or `kill switch` notifications?
- **Skim gateway logs** for new `WARN` or `ERROR` patterns:
  ```bash
  docker compose logs --since 24h gateway | grep -E 'WARN|ERROR' | sort | uniq -c | sort -rn | head
  ```
  New unique patterns are interesting. Repeat offenders you've already categorized are
  usually fine.

### 4.2 Weekly

- **Check the TTL sweeper ran.** Look for the `[boot] goal sweep` log line on each
  gateway restart, and `goals_resumed` on auto-resume. Confirm `scanned` / `purged`
  numbers make sense.
- **Check memory.** Gateway should stay under **150 MB RSS** in steady state:
  ```bash
  docker stats --no-stream
  ```
  If you see > 300 MB and climbing, file an issue — that's a leak.
- **Check Redis disk usage.** A few hundred MB is the ceiling for typical use:
  ```bash
  docker compose exec redis redis-cli info memory | grep used_memory_human
  ```

### 4.3 What to watch for in logs

| Pattern | Severity | Action |
|---|---|---|
| `kill switch active` on Keymaster | INFO | Expected if you activated it. Deactivate when ready |
| `no opportunities found on mainnet — failing goal` | INFO | Normal in low-liquidity windows. Loosen goal strategy or retry |
| `tx rate limit exceeded` | WARN | You're hitting `VESPRA_MAX_TX_PER_HOUR`. Goals are rejected until the hour rolls over. Raise cap if intentional |
| `global wallet cap exceeded` at goal creation | WARN | Your burners collectively hold more than `VESPRA_MAX_GLOBAL_WALLET_VALUE_ETH`. Raise cap or sweep to cold storage |
| `RPC error` / `eth_getBalance failed` | WARN | Your RPC is throttling or flaky. Upgrade tier or change provider |
| Keymaster returns 503 on signing | ERROR | Kill switch on, or disk full preventing state writes. Investigate before resuming |
| Gateway redis connection failed on boot | FATAL | Redis container not healthy; `docker compose logs redis` |
| `BUY tx reverted` | ERROR | On-chain revert — slippage, insufficient liquidity, or bad pool. Check the tx on explorer |

---

## 5. What to Do If Something Goes Wrong

### 5.1 Gateway unresponsive

1. **Activate kill switch directly on Keymaster** (don't wait for the gateway):
   ```bash
   curl -X POST http://localhost:9100/kill-switch/activate \
     -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN"
   ```
2. Confirm with `curl http://localhost:9100/kill-switch/status` → `active: true`.
3. Now investigate. The gateway being dead doesn't touch custody as long as the kill
   switch is on — Keymaster will refuse every signing request.
4. Restart: `docker compose restart gateway`.
5. Deactivate the kill switch only after you've confirmed the gateway is healthy
   (`./scripts/doctor.sh` green).

### 5.2 Funds appear to be missing

1. Get the burner address and check the on-chain explorer first.
2. Match every outbound tx against Keymaster's `/tx/log/<wallet_id>` endpoint. Every
   outbound tx should have a corresponding log entry. A mismatch is a serious finding
   — file a security issue immediately.
3. If all txs are accounted for but the USD value is down, that's market loss, not
   theft. Different problem (see next section on gas / volatility).

### 5.3 Gas spikes / volatility

Pause all goals until the network calms down:

```bash
curl -X POST http://localhost:9001/swarm/kill
```

The kill switch stops new executor calls but leaves existing goals in MONITORING. When
gas normalizes:

```bash
curl -X POST http://localhost:9001/swarm/resume
```

### 5.4 Suspected compromise

1. **Kill switch first.** Worry about root cause second.
2. Disconnect the host from the internet if possible — take it off the network before
   further investigation.
3. Snapshot `/opt/vespra-keymaster/` and the Redis volume before doing anything else.
4. Rotate every secret. See `docs/SECURITY.md` § Incident Response for the full list.
5. Sweep burner funds to a fresh cold wallet **before** rotating Keymaster keys —
   a rotation re-encrypts the keystore, and you don't want to be debugging that
   under pressure.

---

## 6. Scaling Up

Once you have 30+ days of uneventful mainnet operation and are ready to take it
seriously:

### 6.1 Raise the caps gradually

- `VESPRA_MAX_GLOBAL_WALLET_VALUE_ETH`: double it, not 10x. Watch for a week.
- `VESPRA_MAX_TX_PER_HOUR`: 100 is enough for 1–3 concurrent goals. If you run 10+
  goals in parallel with aggressive rotation, raise to 300. Beyond that, you're
  paying for a lot of RPC calls — profile first.
- Per-wallet `cap_eth`: raise via `PUT /wallets/:id/cap`, one wallet at a time.

### 6.2 Parallelize across wallets

Each burner wallet is sandboxed. Running multiple wallets with different strategies
gives you isolation — a bad cycle on one doesn't starve the others of gas.

Practical pattern:

- `mainnet-yield-01` — yield rotation on Aave/Compound, higher capital, longer holds
- `mainnet-trade-01` — trade-up strategy, smaller capital, shorter cycles
- `mainnet-snipe-01` — sniper, tiny capital, event-driven

Each wallet caps the blast radius of any single strategy going wrong.

### 6.3 Consider multiple chains

The same LLM goal parser handles Base + Arbitrum + any chain you configure. Diversify
chain exposure same way you'd diversify asset exposure. But: each chain is one more
RPC endpoint you pay for, one more source of outages, and one more set of tx logs to
reconcile. Scale chain count with operational capacity.

### 6.4 When to *not* scale up

- You're still seeing occasional unexplained `goal failed` — find the root cause first.
- Memory or Redis growth is non-flat over time — leak; file an issue.
- Your RPC provider is throttling you at current volume — upgrade the tier before
  adding more goals.
- You don't have alerts wired to something that wakes you up — Telegram is fine, but
  make sure you'll actually *see* it.

---

## Appendix: Useful Commands

```bash
# goal status
curl -s http://localhost:9001/goals | jq

# specific goal
curl -s http://localhost:9001/goals/<uuid> | jq

# tx log for a wallet
curl -s http://localhost:9100/tx/log/<wallet_id> \
  -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN" | jq

# safeguards status (global cap + rate limit counters)
curl -s http://localhost:9001/safeguards/status | jq

# kill switch status
curl -s http://localhost:9100/kill-switch/status \
  -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN" | jq
```

See [SECURITY.md](./SECURITY.md) for the threat model and incident-response details.
