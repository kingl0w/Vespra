# Vespra Self-Hosted Setup Guide

## Prerequisites

- **VPS**: 2 GB RAM minimum, Ubuntu 22.04+
- **Docker + Docker Compose**: v2.20+ recommended
- **Git**

## Required API Keys

### LLM_API_KEY

An API key for the LLM provider that powers all agent reasoning.

| Provider | Cost | Model | Sign up |
|----------|------|-------|---------|
| **DeepSeek** (recommended) | ~$0.14/M input tokens | `deepseek-chat` | https://platform.deepseek.com |
| OpenAI | ~$2.50/M input tokens | `gpt-4o` | https://platform.openai.com |
| Anthropic | ~$3.00/M input tokens | `claude-sonnet-4-20250514` | https://console.anthropic.com |

DeepSeek is the cheapest option by a wide margin and works well for all Vespra agents.

Set `LLM_PROVIDER` to match your key (`deepseek`, `openai`, or `anthropic`).

### ALCHEMY_API_KEY

Provides RPC access to EVM chains (Base, Arbitrum, Ethereum, Optimism).

- **Where**: https://dashboard.alchemy.com
- **Cost**: Free tier is sufficient for development and light production use (300M compute units/month)
- **Setup**: Create an app, copy the API key. Vespra constructs per-chain RPC URLs automatically.

### ONEINCH_API_KEY vs PARASWAP_MODE

Vespra needs a DEX aggregator for swap quotes and routing.

**Option A: 1inch (default)**
- **Where**: https://portal.1inch.dev
- **Cost**: Free tier available
- **Caveat**: Requires KYC verification, which can take 1-3 days
- Set `ONEINCH_API_KEY=your_key`

**Option B: ParaSwap (no KYC alternative)**
- **Where**: No API key needed
- **Cost**: Free, no signup required
- Set `PARASWAP_MODE=true` and leave `ONEINCH_API_KEY` blank
- Uses https://apiv5.paraswap.io for quotes

If neither is configured, Vespra falls back to simulated quotes (useful for development but not for real trades).

### GNOSIS_SAFE_ADDRESS

A Gnosis Safe multisig that acts as the treasury for sweep-back operations.

- **Where**: https://app.safe.global
- **Setup**: Create a new Safe on your target chain (e.g., Base). Copy the Safe address.
- All Keymaster wallets sweep funds back to this address.

### VESPRA_KM_AUTH_TOKEN

Bearer token for Keymaster (the wallet custody service).

- **Leave blank initially** -- Keymaster auto-generates this on first startup.
- After first `docker compose up`, find the token in the Keymaster logs:
  ```
  docker compose logs keymaster | grep "AUTH_TOKEN"
  ```
- Copy it into your `.env` file, then restart:
  ```
  docker compose restart gateway
  ```

## Quick Start

```bash
# 1. Clone
git clone https://github.com/your-org/vespra.git
cd vespra

# 2. Configure
cp .env.example .env
# Edit .env — fill in LLM_API_KEY, ALCHEMY_API_KEY, and either
# ONEINCH_API_KEY or set PARASWAP_MODE=true

# 3. Launch
docker compose up -d

# 4. Verify
curl http://localhost:9001/health
# Expected: { "status": "ok", ... }
```

The dashboard is available at http://localhost:9200 after startup.

## Optional: Trade Up Loop

The Trade Up loop is a compounding micro-trade strategy that automatically scouts opportunities, enters positions, monitors them, and exits at gain/loss thresholds.

### Configuration

These env vars tune the loop behavior (defaults shown):

| Variable | Default | Description |
|----------|---------|-------------|
| `VESPRA_TRADE_UP_TARGET_GAIN_PCT` | `15` | Exit position when gain reaches this % |
| `VESPRA_TRADE_UP_STOP_LOSS_PCT` | `5` | Exit position when loss reaches this % |
| `VESPRA_TRADE_UP_MAX_ETH` | `0.02` | Max ETH per position |
| `VESPRA_TRADE_UP_GAS_RESERVE_ETH` | `0.01` | ETH reserved for gas (never traded) |

### Activation

Start the loop via the API:

```bash
curl -X POST http://localhost:9001/trade-up/position/start \
  -H "Content-Type: application/json" \
  -d '{"wallet": "my-wallet-label", "chain": "base_sepolia"}'
```

Monitor status:

```bash
curl http://localhost:9001/trade-up/position/status
```

Stop the loop:

```bash
curl -X POST http://localhost:9001/trade-up/position/stop
```

View position history:

```bash
curl http://localhost:9001/trade-up/position/history
```

The loop cycles through: **SCOUTING** (find best token) -> **RISK_CHECK** (gate pass) -> **ENTERING** (swap ETH for token) -> **MONITORING** (poll price every 5 min) -> **EXITING** (swap back to ETH) -> **COMPOUNDING** (reinvest with updated balance, repeat).
