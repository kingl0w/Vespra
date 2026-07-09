# Vespra Setup Guide

## Prerequisites

| Requirement | Minimum |
|---|---|
| VPS or local machine | 2 GB RAM, 2 vCPU, 20 GB disk |
| OS | Ubuntu 22.04+ (or any Linux with Docker) |
| Docker + Docker Compose | v2.20+ |
| Git | any recent version |

If you plan to run from source instead of Docker, you also need:
- Rust 1.85+ (for gateway-rs and keymaster)
- Zig 0.15.2 (for nullboiler)
- Node.js 18+ (for dashboard)
- Redis 7+

## Getting API keys

### LLM provider (required)

An API key for the LLM that powers all agent reasoning (scout, risk, trader, sentinel, etc.).

| Provider | Env vars | Cost | Sign up |
|---|---|---|---|
| **DeepSeek** (recommended) | `LLM_PROVIDER=deepseek`, `LLM_MODEL=deepseek-chat`, `LLM_BASE_URL=https://api.deepseek.com` | ~$0.14/M input tokens | https://platform.deepseek.com |
| OpenAI | `LLM_PROVIDER=openai`, `LLM_MODEL=gpt-4o`, `LLM_BASE_URL=https://api.openai.com` | ~$2.50/M input tokens | https://platform.openai.com |
| Anthropic | `LLM_PROVIDER=anthropic`, `LLM_MODEL=claude-sonnet-4-20250514`, `LLM_BASE_URL=https://api.anthropic.com` | ~$3.00/M input tokens | https://console.anthropic.com |

DeepSeek is the cheapest by a wide margin and works well for all Vespra agents.

Put the key in `LLM_API_KEY` in your `.env` file.

### RPC provider (required for real chains)

Vespra needs RPC URLs to talk to EVM chains. There is **no single `ALCHEMY_API_KEY` variable** -- instead, you set one `RPC_URL_{CHAIN}` variable per chain. The gateway scans all env vars matching the `RPC_URL_*` pattern and lowercases the suffix to get the chain name.

For example, with Alchemy:

1. Go to https://dashboard.alchemy.com and create an app for your chain.
2. Copy the full HTTPS URL (e.g. `https://base-sepolia.g.alchemy.com/v2/abc123`).
3. Put it in `.env`:

```bash
RPC_URL_BASE_SEPOLIA=https://base-sepolia.g.alchemy.com/v2/abc123
RPC_URL_BASE=https://base-mainnet.g.alchemy.com/v2/abc123
RPC_URL_ARBITRUM=https://arb-mainnet.g.alchemy.com/v2/abc123
```

Each URL maps to a chain name: `RPC_URL_BASE_SEPOLIA` becomes the `base_sepolia` chain, `RPC_URL_ARBITRUM` becomes `arbitrum`, etc.

For testnets, free public RPCs work fine:

```bash
RPC_URL_BASE_SEPOLIA=https://sepolia.base.org
RPC_URL_ARBITRUM_SEPOLIA=https://sepolia-rollup.arbitrum.io/rpc
```

### DEX aggregator (one of the two)

Vespra needs a DEX aggregator for swap quotes and routing.

**Option A: ParaSwap (recommended to start)**
- No API key, no signup, no KYC
- Set `PARASWAP_MODE=true` in `.env`
- Uses https://apiv5.paraswap.io

**Option B: 1inch**
- Requires an API key and KYC verification (can take 1-3 days)
- Sign up at https://portal.1inch.dev
- Set `ONEINCH_API_KEY=your_key` in `.env`

If neither is configured, Vespra falls back to simulated quotes (useful for development / testnet, but not for real trades).

### Gnosis Safe (optional)

A Gnosis Safe address used as the treasury for fee sweeps. Only needed if you want fee collection.

- Create a Safe at https://app.safe.global on your target chain
- Set `GNOSIS_SAFE_ADDRESS=0x...` in `.env`
- Keymaster reads this as `VESPRA_SAFE_{CHAIN}` per the compose file

Leave blank if you don't need fee collection.

## Generating secrets

### VESPRA_MASTER_PASSWORD

Encrypts the keystore database where wallet private keys are stored. **Minimum 16 characters.** If you lose this, you lose access to your wallets.

```bash
openssl rand -base64 32
```

Put it in `VESPRA_MASTER_PASSWORD` in `.env`.

### VESPRA_KM_AUTH_TOKEN

Bearer token shared between keymaster (which enforces it) and gateway (which presents it on every request to keymaster).

**This does NOT auto-generate.** Keymaster will exit with an error if `VESPRA_KM_AUTH_TOKEN` is missing or shorter than 16 characters.

Generate it once and put it in `.env` **before** the first `docker compose up`:

```bash
openssl rand -base64 32
```

Both services read the same value from `.env` -- gateway sends it as `Authorization: Bearer <token>`, keymaster checks it on all write endpoints.

## Quick start (Docker)

```bash
# 1. Clone
git clone https://github.com/kingl0w/Vespra.git
cd Vespra

# 2. Configure
cp .env.example .env

# Fill in at minimum:
#   VESPRA_MASTER_PASSWORD  (>= 16 chars, openssl rand -base64 32)
#   VESPRA_KM_AUTH_TOKEN    (>= 16 chars, openssl rand -base64 32)
#   LLM_API_KEY             (from your LLM provider)
#   RPC_URL_BASE_SEPOLIA    (https://sepolia.base.org for testnet)
#   PARASWAP_MODE=true      (already set in .env.example)

# 3. Launch
docker compose up -d

# First build takes a few minutes (Rust + Zig compilation).
# Watch progress with: docker compose logs -f

# 4. Verify all services are healthy
curl http://localhost:9001/health   # gateway
curl http://localhost:9100/health   # keymaster
curl http://localhost:9200          # dashboard
```

The dashboard is at http://localhost:9200.

Services and their ports (all bound to 127.0.0.1 by default):

| Service | Port | Description |
|---|---|---|
| gateway | 9001 | Main REST API, agents, goal runner |
| keymaster | 9100 | Wallet custody, signing, RPC |
| nullboiler | 9090 | DAG workflow engine |
| dashboard | 9200 | Web UI |
| redis | 6379 | State store (internal only) |

## Creating your first wallet

Via curl (or use the dashboard setup wizard):

```bash
TOKEN=$(grep VESPRA_KM_AUTH_TOKEN .env | cut -d= -f2-)

curl -X POST http://localhost:9100/wallets \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "chain": "base_sepolia",
    "label": "my-base-wallet",
    "cap_eth": "0.1"
  }'
```

The response includes the wallet's address. Send some testnet ETH to it from a faucet (e.g. https://www.alchemy.com/faucets/base-sepolia).

**Request fields:**

| Field | Required | Description |
|---|---|---|
| `chain` | yes | Chain name (e.g. `base`, `base_sepolia`, `arbitrum`) |
| `label` | no | Human-readable name, used when submitting goals |
| `cap_eth` | no | Lifetime spend cap in ETH -- keymaster refuses to sign anything that would push the wallet over this. Set small while testing. |
| `strategy` | no | Optional strategy hint |

## Submitting your first goal

```bash
curl -X POST http://localhost:9001/goals \
  -H "Content-Type: application/json" \
  -d '{
    "raw_goal": "Earn yield on 0.001 ETH on Base Sepolia, WETH USDC only",
    "wallet_label": "my-base-wallet"
  }'
```

The response includes a `goal_id` and its initial state.

## Watching a goal progress

Poll the goal's status:

```bash
curl http://localhost:9001/goals/<goal_id>
```

A goal moves through these steps in order:

```
SCOUTING → RISK → TRADING → EXECUTING → MONITORING
```

| Step | What happens |
|---|---|
| **SCOUTING** | Scout agent queries DeFiLlama / pool fetchers for candidates matching your goal. Retries up to 3 times on failure. |
| **RISK** | Risk agent scores each candidate (LOW / MEDIUM / HIGH). HIGH risk = rejected. |
| **TRADING** | Trader agent decides whether to enter based on the risk-cleared candidate. Fetches a swap quote (ParaSwap or 1inch). |
| **EXECUTING** | Executor builds calldata, posts to keymaster `/swap`. Keymaster wraps ETH if needed, approves the router, and sends the transaction. Gateway polls the chain for the receipt. |
| **MONITORING** | Sentinel watches the position. Exits on gain target or stop loss. Yield scheduler checks for better APY and rotates if found. On exit, the goal either completes or cycles back to SCOUTING (for compound/rotate strategies). |

Key fields to watch in the response:

| Field | Meaning |
|---|---|
| `status` | `running`, `completed`, `failed`, `cancelled`, `paused` |
| `current_step` | Which pipeline step the goal is in |
| `pnl_pct` | Current profit/loss percentage |
| `pnl_eth` | Current profit/loss in ETH |
| `error` | Error message if the goal failed |
| `cycles` | How many scout-to-exit loops have completed (for compound strategies) |

Or just open the dashboard and watch the steps tick by in real time.

## Configuration reference

The gateway reads config via [figment](https://docs.rs/figment): first from `config.toml` (if present), then from env vars with the `VESPRA_` prefix. The prefix is stripped and the remainder is lowercased to match struct fields. For example, `VESPRA_REDIS_URL` maps to `redis_url`.

RPC URLs are a special case: `RPC_URL_{CHAIN}` vars (no `VESPRA_` prefix) are scanned separately and populate the `rpc_urls` map.

### Required variables

| Variable | Description |
|---|---|
| `VESPRA_MASTER_PASSWORD` | Keystore encryption password (>= 16 chars) |
| `VESPRA_KM_AUTH_TOKEN` | Shared bearer token for gateway <-> keymaster auth (>= 16 chars) |
| `LLM_API_KEY` | API key for your LLM provider |

### LLM

| Variable | Default | Description |
|---|---|---|
| `LLM_PROVIDER` | `deepseek` | `deepseek`, `openai`, or `anthropic` |
| `LLM_MODEL` | `deepseek-chat` | Model name (must match provider) |
| `LLM_BASE_URL` | `https://api.deepseek.com` | API base URL |

These are set without the `VESPRA_` prefix in `.env`, but the compose file maps them to `VESPRA_LLM_PROVIDER`, `VESPRA_LLM_API_KEY`, etc. for the gateway container.

### RPC URLs

| Variable | Description |
|---|---|
| `RPC_URL_BASE` | RPC for Base mainnet |
| `RPC_URL_BASE_SEPOLIA` | RPC for Base Sepolia testnet |
| `RPC_URL_ARBITRUM` | RPC for Arbitrum |
| `RPC_URL_ARBITRUM_SEPOLIA` | RPC for Arbitrum Sepolia testnet |
| `RPC_URL_ETHEREUM` | RPC for Ethereum mainnet |
| `RPC_URL_OPTIMISM` | RPC for Optimism |

Pattern: `RPC_URL_{CHAIN}` -- any env var matching this pattern is picked up. The suffix is lowercased to form the chain key.

### DEX routing

| Variable | Default | Description |
|---|---|---|
| `PARASWAP_MODE` | `true` | Use ParaSwap for quotes (no API key needed) |
| `ONEINCH_API_KEY` | unset | 1inch API key (alternative to ParaSwap, requires KYC) |

### Gateway service

| Variable | Default | Description |
|---|---|---|
| `VESPRA_HOST` | `127.0.0.1` | Bind address |
| `VESPRA_PORT` | `9000` | Bind port |
| `VESPRA_REDIS_URL` | `redis://127.0.0.1:6379` | Redis connection URL |
| `VESPRA_KEYMASTER_URL` | (empty) | Keymaster base URL (e.g. `http://keymaster:9100`) |
| `VESPRA_KEYMASTER_TOKEN` | (empty) | Same as `VESPRA_KM_AUTH_TOKEN` |
| `VESPRA_NULLBOILER_URL` | `http://127.0.0.1:9090` | NullBoiler base URL |
| `VESPRA_CHAINS` | `["base","arbitrum"]` | JSON array of active chain names |
| `VESPRA_CORS_ORIGIN` | `*` | CORS allowed origin |
| `RUST_LOG` | `info` | Log level (`info`, `debug`, `trace`) |

### Price oracle

| Variable | Default | Description |
|---|---|---|
| `VESPRA_PRICE_ORACLE` | `defillama` | Primary price oracle |
| `VESPRA_PRICE_ORACLE_FALLBACK` | `coingecko` | Fallback price oracle |

### Execution safety

| Variable | Default | Description |
|---|---|---|
| `VESPRA_AUTO_EXECUTE_ENABLED` | `false` | Set `true` to actually broadcast transactions. `false` = dry-run mode. |
| `VESPRA_AUTO_EXECUTE_MAX_ETH` | `0.05` | Max ETH per auto-executed transaction |
| `VESPRA_TRADER_MAX_SLIPPAGE_PCT` | `1.0` | Max slippage tolerance (%) |
| `VESPRA_VOLATILITY_GATE_THRESHOLD` | `15.0` | Reject trades if volatility exceeds this (%) |

### Goal defaults

| Variable | Default | Description |
|---|---|---|
| `VESPRA_TRADE_UP_MAX_ETH` | `0.02` | Max ETH per position |
| `VESPRA_TRADE_UP_STOP_LOSS_PCT` | `5.0` | Default stop loss (%) |
| `VESPRA_TRADE_UP_TARGET_GAIN_PCT` | `15.0` | Default target gain (%) |
| `VESPRA_TRADE_UP_GAS_RESERVE_ETH` | `0.01` | ETH reserved for gas (never traded) |
| `VESPRA_TRADE_UP_CYCLE_INTERVAL_SECS` | `300` | Seconds between compound cycles |
| `VESPRA_TRADE_UP_MIN_GAIN_PCT` | `0.5` | Minimum gain to trigger compound |

### Yield rotation

| Variable | Default | Description |
|---|---|---|
| `VESPRA_YIELD_AUTO_ROTATE_THRESHOLD_PCT` | `1.0` | Min APY delta to trigger rotation |
| `VESPRA_YIELD_MAX_ROTATE_ETH` | `0.05` | Max ETH per rotation |
| `VESPRA_YIELD_CYCLE_INTERVAL_SECS` | `3600` | Seconds between yield checks |

### Sniper

| Variable | Default | Description |
|---|---|---|
| `VESPRA_SNIPER_MAX_ENTRY_ETH` | `0.05` | Max ETH per snipe entry |
| `VESPRA_SNIPER_MIN_TVL` | `50000.0` | Min TVL (USD) to consider a pool |
| `VESPRA_SNIPER_TARGET_GAIN_PCT` | `15.0` | Sniper target gain (%) |
| `VESPRA_SNIPER_STOP_LOSS_PCT` | `8.0` | Sniper stop loss (%) |

### Rate limiting

| Variable | Default | Description |
|---|---|---|
| `VESPRA_RATE_LIMIT_ENABLED` | `true` | Enable rate limiting |
| `VESPRA_RATE_LIMIT_AGENT_RPM` | `10` | Max agent LLM calls per minute |
| `VESPRA_RATE_LIMIT_WALLET_CREATE_RPH` | `5` | Max wallet creates per hour |
| `VESPRA_RATE_LIMIT_TX_SEND_RPH` | `20` | Max transaction sends per hour |

### Fees (optional)

| Variable | Default | Description |
|---|---|---|
| `VESPRA_TREASURY_ADDRESS` | (empty) | Gnosis Safe address for fee sweeps |
| `FEE_RATE_BPS` | `500` | Fee rate in basis points (500 = 5%) |

### Background monitors

| Variable | Default | Description |
|---|---|---|
| `SENTINEL_INTERVAL_SECS` | `300` | How often sentinel polls positions (seconds) |

## Safety

Vespra is built so nothing can lose you more than you've explicitly authorized. The safety layers, in order:

1. **Dry run mode.** `VESPRA_AUTO_EXECUTE_ENABLED=false` (the default). The gateway runs every step including building calldata, but does not broadcast. Set to `true` only when you're ready for real transactions.

2. **Wallet caps.** Every wallet has a `cap_eth` (lifetime spend ceiling). Keymaster refuses to sign anything that would push the wallet over its cap. Set it to whatever you're comfortable losing.

3. **Capital clamp.** Even if the LLM hallucinates a huge `capital_eth` value, the gateway clamps it to 90% of the wallet's cap before creating the goal.

4. **Risk gate.** Every candidate passes through the risk agent before execution. HIGH risk = no trade.

5. **Volatility gate.** If price volatility exceeds `VESPRA_VOLATILITY_GATE_THRESHOLD` (default 15%), the trade is rejected.

6. **Slippage limit.** Trades exceeding `VESPRA_TRADER_MAX_SLIPPAGE_PCT` (default 1%) are rejected.

7. **Stop loss.** The sentinel exits any position that drops past the stop loss percentage.

8. **Kill switch.** `POST /swarm/kill` sets a global flag. While active, no agent will execute anything. Resume with `POST /swarm/resume`. Check status with `GET /swarm/status`.

9. **Rate limiting.** Wallet creation, transaction sends, and agent LLM calls are all rate-limited by default.

**Start on testnet.** Base Sepolia is the easiest -- use `RPC_URL_BASE_SEPOLIA=https://sepolia.base.org`, create a wallet on `base_sepolia`, and fund it from a faucet. The system works identically on testnet, minus real money.
