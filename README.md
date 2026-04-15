# Vespra

Vespra is a self-hosted AI agent system that grows your crypto for you. You give it a plain English goal — *"Earn yield on 0.05 ETH on Base, WETH and USDC only"* — and a swarm of small AI agents handles the rest: finding opportunities, checking risk, executing swaps, monitoring your position, and rotating into better yields when they appear.

It runs on your own machine. Your keys stay on your machine. There's no SaaS, no shared backend, no surprise fees.

## What it does

Point it at a wallet you control and tell it what you want. From there, Vespra:

1. **Reads your goal** with an LLM and turns it into a structured plan (capital, target gain, stop loss, strategy, chain).
2. **Scouts** for the best pool or token matching your goal — APY, liquidity, age, all that.
3. **Runs a risk check** on the result. If it looks shady (low TVL, unverified token, weird pair), it bails.
4. **Executes the swap** through a real DEX — Uniswap V3, Aerodrome, or whatever's wired up for that chain.
5. **Monitors the position** and exits automatically when it hits your gain target or stop loss.
6. **Rotates** into a better yield if one appears (only for yield strategies).
7. **Compounds** the result back into a new cycle if you asked it to.

You watch it work from a web dashboard, or just talk to its REST API.

## The pieces

Vespra is four small services that work together. You can run them all in Docker with one command, or run them as systemd services if you want more control.

### `gateway-rs` — the brain

A Rust HTTP service. This is where the AI agents live and where the goal pipeline runs. It exposes the public REST API on port `9001`.

Inside it you'll find nine agents:

| Agent | Job |
|---|---|
| **scout** | Finds candidate pools / tokens matching your goal |
| **risk** | Grades the candidates (LOW / MEDIUM / HIGH) and rejects bad ones |
| **trader** | Decides whether to enter, hold, or exit |
| **executor** | Builds the actual swap transaction |
| **sentinel** | Watches open positions and triggers exits on gain/loss |
| **yield** | Keeps an eye out for better APY opportunities |
| **sniper** | Detects new pools the moment they launch (for snipe strategies) |
| **coordinator** | Top-level orchestration when multiple goals share state |
| **launcher** | Deploys new tokens (if you're using that strategy) |

Each agent is just a focused LLM prompt + some tool calls. They don't run continuously — they're invoked as the goal moves through its steps.

The gateway also runs two background loops:

- **Sentinel monitor** wakes up every 5 minutes, walks through every active goal, fetches the current price, and decides whether to exit.
- **Yield scheduler** wakes up every 30 minutes and looks for better APY opportunities to rotate into.

### `keymaster` — the wallet vault

A separate Rust service that owns your private keys and is the only thing that ever signs transactions. It exposes its own REST API on port `9100` (loopback only) protected by a bearer token.

The gateway never sees a private key. When it needs to send a transaction, it asks keymaster: *"please swap this WETH for this USDC from this wallet."* Keymaster checks the wallet's spend cap, signs the transaction, broadcasts it, and returns the tx hash.

If somebody compromises the gateway, they can't drain your wallets — keymaster won't sign anything that exceeds the per-wallet cap, and it won't sign anything for an address it doesn't know about.

Wallets are stored encrypted at rest with a master password.

### `nullboiler` — the workflow engine

A small Zig service that runs DAG-style workflows. The gateway uses it for the more complicated multi-step orchestrations (when one goal needs several agent runs in sequence with retries, branching, etc.). For simple yield/compound goals you barely notice it; for advanced workflows it's doing the heavy lifting.

It has its own SQLite database and exposes its own API on port `9090`.

### `dashboard` — the web UI

A React app that talks to the gateway. Lets you:

- Create new goals in plain English
- See the live status of every running goal (current step, PnL, error messages)
- Cancel or pause goals
- Browse wallet balances and the transaction log
- Hit the kill switch if everything's on fire
- Tweak settings (target gain %, stop loss, max ETH per trade, etc.)

It runs on port `9200`.

### `redis` — the glue

Redis holds the live state for goals, the sentinel/yield scheduler signal channels, and short-lived counters. Goals are persisted in Redis (not in-process), so the gateway can be restarted without losing them — on reboot it picks up every running goal exactly where it left off.

## How a goal flows through the system

Walk through a real one. You submit:

```json
POST /goals
{
  "raw_goal": "Earn yield on 0.001 ETH on Base, WETH USDC only",
  "wallet_label": "my-base-wallet"
}
```

What happens next:

1. Gateway resolves `my-base-wallet` to a wallet ID by asking keymaster.
2. Gateway sends the raw goal to the LLM with a parser prompt. LLM returns `{capital_eth: 0.001, strategy: "yield_rotate", chain: "base", target_gain_pct: 10, stop_loss_pct: 5}`.
3. Gateway clamps `capital_eth` against the wallet's spend cap (so the LLM can't blow past your safety limits).
4. Gateway creates the goal in Redis with status `running`, step `SCOUTING`, and spawns a background task to walk it through the pipeline.
5. **Scout** queries DefiLlama / your configured pool fetchers, gets a list of WETH/USDC pools on Base, ranks by APY, returns the top picks.
6. **Risk** runs the candidates through the LLM with a risk prompt, gets a score, drops anything HIGH risk.
7. **Trader** picks the survivor and decides "enter".
8. **Executor** asks the quote fetcher (1inch or Paraswap) for routing, builds the calldata, posts to keymaster `/swap`.
9. Keymaster wraps ETH → WETH if needed, approves the router, sends the actual `exactInputSingle` swap, and returns a tx hash.
10. Gateway polls the chain RPC for the receipt. Once confirmed, the goal moves to `MONITORING`.
11. **Sentinel** wakes up every 5 minutes and checks the current price. If you've gained 10% it triggers exit. If you've lost 5% it triggers stop loss.
12. **Yield scheduler** wakes up every 30 minutes and asks: "is there a better APY pool than what we're in?" If yes, it publishes a rotation signal and the goal moves into a swap-out, swap-in cycle.

You can watch all of this live in the dashboard, or by polling `GET /goals/{id}`.

## Strategies

You don't pick a strategy explicitly — the LLM picks it from your prompt. There are four:

- **`yield_rotate`** — find the best APY pool, hold, monitor, rotate when a better one appears. *"Earn yield on X"*
- **`compound`** — accumulate gains and re-enter with the bigger pot. *"Compound my ETH"*
- **`snipe`** — watch for newly launched pools and enter the moment they pass risk checks. *"Snipe new pools on Base"*
- **`adaptive`** — generic grow-the-bag mode that picks tactics on the fly. *"Grow 0.05 ETH"*

## Setting it up

There's a more detailed walkthrough in [`docs/SETUP.md`](docs/SETUP.md). The fast version:

### What you need

- A VPS or local machine, 2GB RAM minimum, Ubuntu 22.04+
- Docker + Docker Compose (recommended) **or** Rust toolchain + Zig if you want to run from source
- An LLM API key — DeepSeek is by far the cheapest and works fine
- An RPC provider — Alchemy free tier is enough
- A DEX aggregator — either a 1inch API key (needs KYC) or just set `PARASWAP_MODE=true` (no signup)
- A funded wallet on the chain you want to trade on. **Start on a testnet.** Base Sepolia is the easiest.

### The fast path (Docker)

```bash
git clone https://github.com/your-org/vespra.git
cd vespra

cp .env.example .env
# edit .env — at minimum fill in these three:
#   VESPRA_MASTER_PASSWORD=something-long-and-random   (>=16 chars)
#   VESPRA_KM_AUTH_TOKEN=$(openssl rand -base64 32)
#   LLM_API_KEY=sk-...

docker compose up -d

# wait ~30 seconds for the Rust services to build the first time
curl http://localhost:9001/health
curl http://localhost:9100/health
```

The dashboard is at <http://localhost:9200>.

`VESPRA_KM_AUTH_TOKEN` is shared between keymaster (which enforces it) and gateway (which presents it on every call). Generate one once and put it in `.env` before `docker compose up` — both services read the same value.

### Creating a wallet

The dashboard has a setup wizard. Or via curl:

```bash
TOKEN=$(grep VESPRA_KM_AUTH_TOKEN .env | cut -d= -f2-)

curl -X POST http://localhost:9100/wallets \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "label": "my-base-wallet",
    "chain": "base_sepolia",
    "cap_eth": "0.1"
  }'
```

That returns a fresh address. Send some testnet ETH to it from a faucet (e.g. <https://www.alchemy.com/faucets/base-sepolia>).

The `cap_eth` field is your safety net: keymaster will refuse to sign anything that would push the wallet's lifetime spend over this number. Set it small while you're testing.

### Submitting your first goal

```bash
curl -X POST http://localhost:9001/goals \
  -H "Content-Type: application/json" \
  -d '{
    "raw_goal": "Earn yield on 0.001 ETH on Base Sepolia, WETH USDC only",
    "wallet_label": "my-base-wallet"
  }'
```

You'll get back a goal ID. Watch it progress:

```bash
curl http://localhost:9001/goals/<id>
```

Or just open the dashboard and watch the steps tick by.

## Configuration that actually matters

Most of the env vars in `.env` have sensible defaults. The ones you'll probably want to know about:

| Variable | What it does | Default |
|---|---|---|
| `LLM_PROVIDER` | `deepseek`, `openai`, or `anthropic` | `deepseek` |
| `LLM_MODEL` | model name | `deepseek-chat` |
| `VESPRA_PARASWAP_MODE` | use ParaSwap (no API key) for quotes | `true` |
| `VESPRA_ONEINCH_API_KEY` | alternative to ParaSwap | unset |
| `RPC_URL_BASE` | RPC for Base mainnet | unset |
| `RPC_URL_BASE_SEPOLIA` | RPC for Base Sepolia testnet | unset |
| `RPC_URL_ARBITRUM` | RPC for Arbitrum | unset |
| `VESPRA_AUTO_EXECUTE_ENABLED` | actually broadcast txs (vs dry-run) | `false` |
| `SENTINEL_INTERVAL_SECS` | how often the sentinel polls | `300` |
| `YIELD_CHECK_INTERVAL_SECS` | how often the yield scheduler runs | `1800` |
| `VESPRA_KM_AUTH_TOKEN` | bearer token for keymaster (shared with gateway) | required |
| `VESPRA_MASTER_PASSWORD` | encrypts the keystore (>=16 chars) | required |

RPC URLs follow the pattern `RPC_URL_{CHAIN}` — the suffix is lowercased and used as the chain key. So `RPC_URL_BASE_SEPOLIA` populates the `base_sepolia` chain, `RPC_URL_ARBITRUM` populates `arbitrum`, etc.

## Safety

Vespra is built so that nothing can lose you more money than you've explicitly authorized. The layers, in order:

1. **Wallet caps**. Every wallet has a `cap_eth` (lifetime spend ceiling). Keymaster refuses to sign if a transaction would push the wallet over its cap. Set this to whatever you're comfortable losing.
2. **Capital clamp**. Even if the LLM hallucinates a giant `capital_eth` value, the gateway clamps it to 90% of the wallet's cap before submitting the goal.
3. **Risk gate**. Every position passes through the risk agent before execution. HIGH risk = no trade.
4. **Stop loss**. The sentinel exits any position that drops past your stop loss percentage, no questions asked.
5. **Kill switch**. The dashboard has a big red button that sets a global flag. While the flag is on, no agent will execute anything. You can also flip it via the API: `POST /kill-switch/on`.
6. **Dry run mode**. Set `VESPRA_AUTO_EXECUTE_ENABLED=false` and the gateway will go through every step including building the calldata, but will *not* broadcast. Useful for testing prompts without spending real gas.

It is still your money. Start small, run on testnet first, watch the logs, and don't trust an LLM with more capital than you'd be okay losing.

## Project layout

```
vespra/
├── gateway-rs/        # Rust gateway — agents, goal runner, schedulers, REST API
├── keymaster/         # Rust wallet custody service
├── nullboiler/        # Zig DAG workflow engine
├── dashboard/         # React frontend
├── redis/             # Redis config
├── docker-compose.yml # The whole stack in one file
└── docs/
    └── SETUP.md       # Detailed setup walkthrough
```

If you want to read code, the most interesting starting points are:

- `gateway-rs/src/goal_runner.rs` — the main pipeline that walks a goal through SCOUTING → EXECUTING → MONITORING
- `gateway-rs/src/agents/` — one file per agent, each is a focused LLM wrapper
- `gateway-rs/src/sentinel_monitor.rs` — the background loop that watches positions
- `gateway-rs/src/yield_scheduler.rs` — the background loop that rotates yields
- `keymaster/src/swap.rs` — the wrap → approve → swap flow on Uniswap V3

## Running from source (no Docker)

If you want to develop on the gateway or keymaster:

```bash
# keymaster
cd keymaster
cargo build --release
./target/release/vespra-keymaster

# gateway-rs (in another terminal)
cd ../gateway-rs
cargo build --release
./target/release/gateway-rs

# nullboiler
cd ../nullboiler
zig build
./zig-out/bin/nullboiler --port 9090 --db nullboiler.db --config config.json

# dashboard
cd ../dashboard
npm install
npm run dev
```

You'll need a Redis running on `localhost:6379` either way.

## License

MIT — see [`LICENSE`](LICENSE).
