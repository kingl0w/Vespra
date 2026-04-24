# Security Model

Vespra manages signing keys for real on-chain funds. This document describes what the
system is designed to resist, what it explicitly does not defend against, and how to
respond if something goes wrong.

**This software has not been externally audited.** Run mainnet with money you can
afford to lose. See also [MAINNET.md](./MAINNET.md) for the operational transition
guide.

---

## 1. Threat Model

### What Vespra is designed to resist

- **Gateway compromise.** The Keymaster kill switch is the last line of defense. An
  attacker who achieves full RCE on the gateway container cannot drain wallets while
  the kill switch is active — Keymaster refuses every signing request with a 503
  regardless of what the gateway asks. The kill-switch state is persisted to disk and
  re-loaded on restart.
- **Accidental runaway execution.** Per-wallet `cap_eth` caps (enforced by Keymaster),
  gateway-wide `VESPRA_MAX_TX_PER_HOUR`, and the optional global
  `VESPRA_MAX_GLOBAL_WALLET_VALUE_ETH` bound the blast radius when an LLM hallucinates
  a large value, an oracle returns bad data, or a loop gets stuck.
- **Restart data loss.** Redis persists every goal through state transitions.
  `VESPRA_AUTO_RESUME_GOALS=true` re-enters the lifecycle on boot, routing
  mid-execution crashes either back to SCOUTING (if no token was persisted) or forward
  to MONITORING (if the BUY landed).
- **Observation of sensitive values.** Private keys are encrypted at rest with
  AES-256-GCM, derived from the master password via Argon2id. Keys are only decrypted
  in-memory when a signing request arrives. Sensitive environment variables
  (`KEYMASTER_MASTER_PASSWORD`, `KEYMASTER_BEARER_TOKEN`, `ANTHROPIC_API_KEY`,
  `VESPRA_TELEGRAM_BOT_TOKEN`) are never logged, echoed in error messages, or included
  in HTTP responses.
- **Unauthorized API access.** Every write endpoint on Keymaster requires a Bearer
  token. Public read endpoints (wallet list, balances, health) do not expose secrets.

### What Vespra does NOT defend against

- **Physical or root access to the server.** Root can read decrypted master password
  from gateway memory during a signing request, read `/proc/<pid>/environ`, or modify
  the kill-switch state file. If root is compromised, treat all wallets as compromised.
- **Compromised Anthropic API key.** Lets an attacker shape agent decisions via
  prompt injection against goals you submit. Rotate on any suspicion.
- **Compromised RPC provider.** A malicious RPC can lie about `eth_call` quotes, pool
  reserves, and balances. Vespra does limited cross-checks (quote vs. post-swap
  balance) but is not a full oracle defender. Use a reputable paid provider.
- **Smart contract exploits in upstream protocols.** If Uniswap, Aave, Compound, or
  whatever pool the scout picked is exploited, funds there can be lost. The risk
  agent's TVL / age / audit heuristics are advisory, not guarantees.
- **MEV sandwich attacks.** Slippage tolerance is partial mitigation. The gateway
  does not use private mempools today; at sizes where MEV matters, route through
  Flashbots or similar yourself.
- **Social engineering.** Telegram tokens, Anthropic keys, and RPC keys can all be
  phished. The software can't protect against you pasting them into the wrong place.

---

## 2. Security Primitives

### Encryption at rest

- **Algorithm:** AES-256-GCM
- **Key derivation:** Argon2id over `KEYMASTER_MASTER_PASSWORD`
- **Scope:** Every burner-wallet private key in `keymaster.db`. Nothing else is
  encrypted (metadata, labels, addresses are plaintext by design for observability).

### Authentication

- **Bearer token** on all Keymaster write endpoints (`/tx/*`, `/swap`, `/wallets*`,
  `/kill-switch/activate`, `/kill-switch/deactivate`, `/dispatch`).
- **Public read endpoints** (`/health`, `/wallets` (GET), `/balance/*`) do not require
  auth and do not return secret material.
- The gateway ships the bearer token on every outbound Keymaster request; it lives in
  `VESPRA_KEYMASTER_TOKEN` (mirror of `KEYMASTER_BEARER_TOKEN`).

### Kill switch

- **Enforced at Keymaster**, not the gateway. A gateway compromise cannot bypass it.
- **Persisted** to `/opt/vespra-keymaster/kill-switch.state` (path overridable via
  `VESPRA_KM_KILL_SWITCH_STATE`). Survives restart.
- **Gateway propagation:** `POST /swarm/kill` on the gateway calls Keymaster's
  `/kill-switch/activate` first; if Keymaster is unreachable, the gateway returns 502
  and the gateway-local flag is **not** set. Keymaster is the source of truth.

### Rate & cap controls

- **Per-wallet `cap_eth`** — Keymaster refuses sends exceeding the wallet's cap; even
  if the gateway asks for 10 ETH, Keymaster short-circuits.
- **Gateway `VESPRA_MAX_TX_PER_HOUR`** — Redis sliding-window counter
  (`vespra:tx_rate:<hour_bucket>`) checked before every executor call. Excess requests
  fail the goal cleanly with a `tx rate limit exceeded` error.
- **Global `VESPRA_MAX_GLOBAL_WALLET_VALUE_ETH`** — sums balances across all burners
  at goal-creation time and rejects with 400 if the proposed capital would push total
  custody over the cap.

### Input handling

- **LLM inputs** (goal text) are sandboxed — the parser returns a structured
  `GoalSpec` with type-validated fields (chain, capital_eth, strategy, thresholds). A
  prompt-injected goal cannot request an out-of-schema action.
- **Chain and token symbols** are resolved against a fixed registry; unknown chains
  fail fast. Token addresses must pass EIP-55 checksum validation before reaching
  Keymaster.
- **Error messages** are sanitized server-side — RPC internals, reqwest URLs, and
  upstream stack traces are redacted from responses and replaced with generic
  descriptions.

---

## 3. Known Limitations

- **Not third-party audited.** This is pre-audit software. Dual-review the code if
  you're running more than pocket change.
- **Testnet-only code paths exist in the binary.** The synthetic WETH/USDC fallback
  opportunity injection (used when DeFiLlama returns nothing on Sepolia) is gated
  behind `config.is_testnet()`. It is dead code on mainnet, but the fact that it
  exists in the binary means a future bug that inverts the `is_testnet()` check could
  resurrect it. Audit diffs that touch `safeguards.rs` or `goal_runner.rs` carefully.
- **Kill-switch state is on local disk.** Disk corruption leaves the switch defaulting
  to off on next boot. If you're on unreliable storage, snapshot the state file
  periodically.
- **Logs contain goal IDs, wallet addresses, and transaction hashes.** Never private
  keys, master passwords, or API keys. Still, handle log retention accordingly — a
  goal ID + address pair lets an observer correlate your on-chain activity with your
  Vespra instance.
- **Co-located Keymaster + gateway.** By default, both run on the same Docker host.
  A root-level compromise of the host reduces the kill-switch guarantee (the attacker
  can read Keymaster state directly). Deploying Keymaster on a separate host with its
  own network namespace is not supported out-of-the-box but is a reasonable hardening
  step for high-value deployments.
- **No HSM integration.** Private keys live in a SQLite file, encrypted. A hardware
  wallet / HSM path is not implemented.
- **No formal access review for the dashboard.** The dashboard is a thin SPA over the
  gateway's public REST API. If you expose the gateway publicly, the dashboard
  exposes everything the gateway does. Put it behind Cloudflare Access or similar.

---

## 4. Incident Response

If you suspect compromise — even without certainty — work through this list in order.
Speed matters more than completeness.

### Step 1: Kill switch first

```bash
curl -X POST http://localhost:9100/kill-switch/activate \
  -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN"
```

Verify:

```bash
curl -s http://localhost:9100/kill-switch/status \
  -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN" | jq
# → {"active": true, "activated_at": "..."}
```

### Step 2: Isolate the host

If you can, unplug the network interface or take the VPS off the internet via the
provider console. Don't shut down — you'll lose ephemeral state that's useful for
forensics.

### Step 3: Snapshot everything before you change anything

```bash
# keymaster state + DB
sudo tar czf /tmp/km-snapshot-$(date +%s).tar.gz /opt/vespra-keymaster/

# redis dump
docker compose exec redis redis-cli save
docker cp $(docker compose ps -q redis):/data /tmp/redis-snapshot-$(date +%s)

# logs
docker compose logs > /tmp/logs-$(date +%s).txt
```

Store snapshots off-host. You'll want them whether this turns out to be a software
bug or an actual intrusion.

### Step 4: Sweep funds before rotating keys

Rotating Keymaster's master password requires re-encrypting the keystore — don't do
that with an attacker potentially still on the host. Sweep every burner to a fresh
cold wallet first:

```bash
for wid in $(curl -s http://localhost:9100/wallets | jq -r '.[].wallet_id'); do
  curl -X POST http://localhost:9100/tx/sweep \
    -H "Authorization: Bearer $KEYMASTER_BEARER_TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"wallet_id\":\"$wid\"}"
done
```

This requires the kill switch to be **off** temporarily. Trade-off: you can't sweep
while it's on. If you're confident the attacker can't redirect sweeps (e.g. you've
already pulled the network cable and are running sweeps through a trusted RPC via a
different interface), turn it off briefly, sweep, turn it back on. If you're not
confident, accept the loss exposure and rotate instead.

### Step 5: Rotate every secret

- `KEYMASTER_MASTER_PASSWORD` — requires re-encrypting every keystore entry. No
  built-in script yet; the rotation procedure today is: decrypt + re-encrypt with
  new password, then update `.env`. Plan this carefully or do it in a test instance
  first.
- `KEYMASTER_BEARER_TOKEN` — change in `.env`, `make restart`.
- `ANTHROPIC_API_KEY` — revoke in the Anthropic dashboard, generate a new key, update
  `.env`.
- `VESPRA_TELEGRAM_BOT_TOKEN` — revoke in @BotFather (`/revoke`), generate new.
- **All burner wallet keys** — already rotated implicitly by Step 4's sweep + fresh
  wallet creation. Confirm no funds remain on the old burners on-chain.

### Step 6: Investigate

Search logs for anomalies:

```bash
# unusual keymaster 503s (kill switch fires)
grep 'kill switch active' /tmp/logs-*.txt

# unexpected /wallets POSTs (wallet creation)
grep 'Created new burner wallet' /tmp/logs-*.txt

# auth failures (attacker probing)
grep 'Auth failed' /tmp/logs-*.txt
```

Timeline the events. If the compromise vector is a software bug, file a security
advisory. If it's operational (leaked token, compromised API key), update your own
procedures.

---

## 5. Responsible Disclosure

Found a vulnerability? **Do not file a public issue.**

- Use GitHub's private security advisory flow on this repo
  (`Security → Report a vulnerability`)
- We aim to respond within **72 hours**
- We'll credit reporters in the fix commit unless you request otherwise

What's in scope:

- Any path that lets a non-authenticated caller trigger a signing operation
- Any path that lets a gateway compromise bypass the Keymaster kill switch
- Private key material leaking through logs, error messages, or API responses
- Authentication bypass on Keymaster protected endpoints
- Persistence of the kill switch being defeated (e.g. a way to silently clear it)

What's out of scope:

- Reports that "the LLM made a bad decision" — the threat model accepts that
- Issues in upstream protocols (Uniswap, Aave, etc.)
- Problems that require physical access / root on the host
- MEV / sandwich attacks against public-mempool swaps

Thank you for reporting responsibly.
