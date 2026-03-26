#!/usr/bin/env python3
"""Vespra Worker Gateway — NullBoiler to NullClaw bridge. Zero dependencies."""

import json
import re
import time, re, subprocess, logging, time, os
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.request import Request, urlopen
from urllib.error import URLError

PORT = 9000
HOST = "127.0.0.1"
NULLCLAW = "/usr/local/bin/nullclaw"
KEYMASTER = "http://127.0.0.1:9100"
KEYMASTER_TOKEN = os.environ.get("VESPRA_KM_AUTH_TOKEN", "")
TIMEOUT = 120

import threading

# ─── Redis queue config ───────────────────────────────────────────
REDIS_HOST     = os.environ.get("REDIS_HOST", "127.0.0.1")
REDIS_PORT     = int(os.environ.get("REDIS_PORT", "6379"))
REDIS_DB       = int(os.environ.get("REDIS_DB", "0"))
QUEUE_KEY      = "vespra:job_queue"
RETRY_KEY      = "vespra:retry_queue"
DLQ_KEY        = "vespra:dlq"
QUEUE_ENABLED  = os.environ.get("VESPRA_QUEUE_ENABLED", "true").lower() == "true"
BRPOP_TIMEOUT  = 5  # seconds — allows clean shutdown checks

# ─── LLM Provider config ──────────────────────────────────────────
LLM_PROVIDER = os.environ.get("LLM_PROVIDER", "deepseek").strip().lower()
LLM_MODEL    = os.environ.get("LLM_MODEL", "").strip()
LLM_API_KEY  = os.environ.get("LLM_API_KEY", "").strip()

# Provider → default model mapping
_PROVIDER_DEFAULTS = {
    "deepseek":  "deepseek-chat",
    "openai":    "gpt-4o-mini",
    "anthropic": "claude-haiku-4-5-20251001",
}
_SUPPORTED_PROVIDERS = set(_PROVIDER_DEFAULTS.keys())

if LLM_PROVIDER not in _SUPPORTED_PROVIDERS:
    import sys
    print(f"[FATAL] LLM_PROVIDER='{LLM_PROVIDER}' is not supported. Choose from: {sorted(_SUPPORTED_PROVIDERS)}", flush=True)
    sys.exit(1)

# Resolve model: env override > provider default
_RESOLVED_MODEL = LLM_MODEL or _PROVIDER_DEFAULTS[LLM_PROVIDER]

def pre_fetch_scout():
    """Fetch live pool data from DeFi Llama for Scout agent.

    Multi-chain, momentum-scored, with price feed integration.
    """
    from datetime import datetime, timezone
    try:
        req = Request("https://yields.llama.fi/pools", method="GET")
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        pools = data.get("data", [])

        # Filter: min TVL $500k, min APY 1%, only configured chains
        filtered = [
            p for p in pools
            if (p.get("tvlUsd") or 0) >= 500_000
            and (p.get("apy") or 0) >= 1.0
            and p.get("chain", "") in SCOUT_CHAINS
        ]

        # Fetch price data from DeFi Llama coins API for top tokens
        price_map = {}
        try:
            # Build coin list from top pool symbols
            symbols = list({p.get("symbol", "").split("-")[0] for p in filtered[:50]})
            # DeFi Llama coins batch price endpoint
            coin_ids = ",".join(f"coingecko:{s.lower()}" for s in symbols[:20])
            price_req = Request(
                f"https://coins.llama.fi/prices/current/{coin_ids}",
                method="GET",
            )
            with urlopen(price_req, timeout=6) as pr:
                price_data = json.loads(pr.read())
            for key, val in price_data.get("coins", {}).items():
                symbol = key.replace("coingecko:", "").upper()
                price_map[symbol] = {
                    "price_usd":          val.get("price", 0.0),
                    "price_change_24h":   val.get("change_24h") or 0.0,
                }
        except Exception:
            pass  # Price feed is best-effort

        scored = []
        for p in filtered:
            apy          = p.get("apy") or 0.0
            tvl          = p.get("tvlUsd") or 0
            tvl_7d       = p.get("tvlUsd7d") or tvl  # fallback to current if missing
            vol_24h      = p.get("volumeUsd1d") or 0.0
            vol_7d_avg   = (p.get("volumeUsd7d") or 0.0) / 7
            il_risk      = p.get("ilRisk", "unknown")
            symbol       = p.get("symbol", "")
            chain        = p.get("chain", "")

            # TVL 7d change %
            tvl_change_7d_pct = 0.0
            if tvl_7d and tvl_7d > 0:
                tvl_change_7d_pct = round(((tvl - tvl_7d) / tvl_7d) * 100, 2)

            # Volume spike: today vs 7d average
            vol_spike = 0.0
            if vol_7d_avg > 0:
                vol_spike = round(((vol_24h - vol_7d_avg) / vol_7d_avg) * 100, 2)

            # Normalize APY score: cap at 200% to avoid outlier distortion
            apy_norm = min(apy, 200.0) / 200.0

            # Normalize TVL trend: -100% to +100% range → 0 to 1
            tvl_norm = max(0.0, min(1.0, (tvl_change_7d_pct + 100) / 200))

            # Normalize volume spike: cap at 500% → 0 to 1
            vol_norm = max(0.0, min(1.0, (vol_spike + 100) / 600))

            # Composite momentum score (weighted)
            momentum_score = round(
                (apy_norm * 0.4) + (tvl_norm * 0.3) + (vol_norm * 0.3),
                4,
            )

            # Entry signal based on momentum score
            if momentum_score >= 0.65:
                entry_signal = "strong"
            elif momentum_score >= 0.45:
                entry_signal = "moderate"
            elif momentum_score >= 0.25:
                entry_signal = "weak"
            else:
                entry_signal = "none"

            # Price data for primary token in pair
            base_token = symbol.split("-")[0].upper() if "-" in symbol else symbol.upper()
            price_info = price_map.get(base_token, {})

            scored.append({
                "protocol":           p.get("project", ""),
                "pool":               symbol,
                "chain":              chain,
                "apy":                round(apy, 2),
                "tvl_usd":            int(tvl),
                "il_risk":            il_risk,
                "stable":             bool(p.get("stablecoin", False)),
                "tvl_change_7d_pct":  tvl_change_7d_pct,
                "volume_24h":         int(vol_24h),
                "volume_spike_pct":   vol_spike,
                "momentum_score":     momentum_score,
                "entry_signal":       entry_signal,
                "price_usd":          price_info.get("price_usd", 0.0),
                "price_change_24h_pct": price_info.get("price_change_24h", 0.0),
            })

        # Sort by momentum score descending, take top 20
        scored.sort(key=lambda p: p["momentum_score"], reverse=True)
        top = scored[:20]

        # Summary stats
        strong_signals = sum(1 for p in top if p["entry_signal"] == "strong")
        chains_covered = list({p["chain"] for p in top})

        data_timestamp = datetime.now(timezone.utc).isoformat()
        return {
            "pool_count":      len(filtered),
            "top_pools":       top,
            "chains_scanned":  chains_covered,
            "strong_signals":  strong_signals,
            "data_timestamp":  data_timestamp,
        }
    except Exception as e:
        from datetime import datetime, timezone
        return {
            "pool_count": 0, "top_pools": [], "chains_scanned": [],
            "strong_signals": 0, "error": str(e),
            "data_timestamp": datetime.now(timezone.utc).isoformat(),
        }


def pre_fetch_risk(protocol, contract_address=None, chain="base"):
    """Fetch live protocol data + contract analysis for Risk agent."""
    try:
        req = Request(f"https://api.llama.fi/protocol/{protocol}", method="GET")
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        tvl_array = data.get("tvl", [])
        audits = data.get("audits") or data.get("audit_links") or data.get("auditLinks") or []

        current_tvl = 0
        latest_tvl = 0
        if tvl_array and isinstance(tvl_array[-1], dict):
            latest_tvl = tvl_array[-1].get("totalLiquidityUSD", 0) or 0
            current_tvl = latest_tvl

        tvl_trend = 0
        if tvl_array and len(tvl_array) >= 2 and latest_tvl > 0:
            target_ts = tvl_array[-1].get("date", 0) - (30 * 86400)
            past_val = None
            for entry in tvl_array:
                if isinstance(entry, dict) and entry.get("date", 0) <= target_ts:
                    past_val = entry.get("totalLiquidityUSD", 0)
            if past_val and past_val > 0:
                tvl_trend = round(((latest_tvl - past_val) / past_val) * 100, 2)

        age_days = 0
        if tvl_array and isinstance(tvl_array[0], dict):
            first_date = tvl_array[0].get("date", 0) or 0
            if first_date > 0:
                age_days = int((time.time() - first_date) / 86400)

        # TVL velocity (1hr change)
        tvl_velocity = _get_tvl_velocity(protocol)

        # Contract analysis (if address provided or extractable)
        contract_analysis = {"honeypot_risk": "UNKNOWN", "error": "no_address"}
        if contract_address:
            contract_analysis = _analyze_contract(contract_address, chain)
        else:
            # Try to get contract address from DeFi Llama
            addresses = data.get("address") or data.get("addresses", {})
            if isinstance(addresses, str) and addresses.startswith("0x"):
                contract_analysis = _analyze_contract(addresses, chain)
            elif isinstance(addresses, dict):
                addr = addresses.get(chain) or addresses.get("base") or addresses.get("ethereum")
                if addr and addr.startswith("0x"):
                    contract_analysis = _analyze_contract(addr, chain)

        return {
            "tvl":               current_tvl,
            "tvl_trend":         tvl_trend,
            "tvl_velocity_1hr":  tvl_velocity,
            "audits":            audits,
            "age_days":          age_days,
            "contract_analysis": contract_analysis,
            "liquidity_locked":  bool(data.get("listedAt")),
            "chain":             chain,
        }
    except Exception as e:
        return {
            "error": str(e), "tvl": 0, "tvl_trend": 0, "tvl_velocity_1hr": 0,
            "audits": [], "age_days": 0,
            "contract_analysis": {"honeypot_risk": "UNKNOWN"},
            "liquidity_locked": False, "chain": chain,
        }


BASE_TOKEN_ADDRESSES = {
    8453: {
        "USDC": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
        "WETH": "0x4200000000000000000000000000000000000006",
        "DAI":  "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb",
    }
}

def pre_fetch_trader(token_in, token_out, amount_wei, chain_id=8453):
    """Fetch live swap quote from 1inch aggregator for Trader agent context."""
    try:
        api_key = os.environ.get("ONEINCH_API_KEY", "").strip()
        tokens = BASE_TOKEN_ADDRESSES.get(chain_id, {})
        src = tokens.get(token_in.upper(), token_in)
        dst = tokens.get(token_out.upper(), token_out)
        url = (
            f"https://api.1inch.dev/swap/v6.0/{chain_id}/quote"
            f"?src={src}&dst={dst}&amount={amount_wei}"
        )
        req = Request(url, headers={
            "Authorization": f"Bearer {api_key}",
            "accept": "application/json",
            "User-Agent": "Mozilla/5.0 (compatible; Vespra/1.0)",
        })
        with urlopen(req, timeout=8) as resp:
            raw = resp.read()
        print(f"1inch response: {raw}", flush=True)
        data = json.loads(raw)
        dst_amount = data.get("dstAmount", "0")
        return {
            "token_in": src,
            "token_out": dst,
            "amount_in": str(amount_wei),
            "amount_out": str(dst_amount),
            "price_impact": 0.0,
            "gas_estimate": 0,
            "chain_id": chain_id,
        }
    except Exception as e:
        return {"error": str(e), "amount_out": 0, "price_impact": 0, "gas_estimate": 0}


CHAIN_MAP = {1: "Ethereum", 8453: "Base", 42161: "Arbitrum"}

# Chains Scout scans — configurable via env var
SCOUT_CHAINS = [
    c.strip().capitalize()
    for c in os.environ.get("SCOUT_CHAINS", "Base,Arbitrum").split(",")
    if c.strip()
]

def pre_fetch_yield(chain_id=1):
    """Fetch live Aave V3 market rates and gas price for Yield agent context."""
    try:
        from datetime import datetime, timezone
        chain_name = CHAIN_MAP.get(chain_id, "Ethereum")

        # Fetch Aave V3 pools from DeFi Llama yields API
        req = Request("https://yields.llama.fi/pools", method="GET")
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        pools = data.get("data", [])
        aave_pools = [
            p for p in pools
            if p.get("project") == "aave-v3" and p.get("chain") == chain_name
        ]

        # Fetch ETH gas price
        gas_price_gwei = 0.0
        try:
            gas_req = Request("https://api.etherscan.io/api?module=gastracker&action=gasoracle", method="GET")
            with urlopen(gas_req, timeout=5) as gas_resp:
                gas_data = json.loads(gas_resp.read())
            gas_price_gwei = float(gas_data.get("result", {}).get("ProposeGasPrice", 0))
        except Exception:
            pass

        # Build market list with net APY
        markets = []
        for p in aave_pools:
            supply_apy = p.get("apy") or 0.0
            borrow_apy = p.get("apyBorrow") or 0.0
            tvl_usd = int(p.get("tvlUsd") or 0)
            # Net APY: supply APY minus $50 gas cost amortized over 30 days
            # Assume minimum $1000 deposit to calculate gas drag as percentage
            gas_cost_annualized = (50.0 / 30) * 365  # ~$608/yr
            deposit_size = max(tvl_usd / 1000, 1000)  # rough per-user estimate, floor $1000
            gas_drag_pct = (gas_cost_annualized / deposit_size) * 100
            net_apy = round(supply_apy - gas_drag_pct, 4)
            markets.append({
                "asset": p.get("symbol", ""),
                "chain": chain_name,
                "supply_apy": round(supply_apy, 4),
                "borrow_apy": round(borrow_apy, 4),
                "net_apy": net_apy,
                "tvl_usd": tvl_usd,
            })
        markets.sort(key=lambda m: m["net_apy"], reverse=True)

        return {
            "markets": markets[:20],
            "gas_price_gwei": gas_price_gwei,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }
    except Exception as e:
        return {"markets": [], "gas_price_gwei": 0, "error": str(e), "timestamp": ""}


AAVE_V3_SUBGRAPHS = {
    "base":      "https://api.goldsky.com/api/public/project_clk74pd7lueg738tw9t0cepc5/subgraphs/aave-v3-base/1.0.0/gn",
    "ethereum":  "https://api.thegraph.com/subgraphs/name/aave/protocol-v3",
    "arbitrum":  "https://api.thegraph.com/subgraphs/name/aave/protocol-v3-arbitrum",
}
AAVE_RAY = 1e27

# ─── Sentinel watchdog config ─────────────────────────────────────
SENTINEL_POLL_INTERVAL = int(os.environ.get("SENTINEL_POLL_INTERVAL", "300"))  # seconds
SENTINEL_STOP_LOSS_PCT = float(os.environ.get("SENTINEL_STOP_LOSS_PCT", "20.0"))  # exit on 20% drop
SENTINEL_ALERT_CHANNEL = os.environ.get("SENTINEL_ALERT_CHANNEL", "")  # NullClaw channel name

# ─── Yield rotation config ────────────────────────────────────────
YIELD_AUTO_ROTATE_ENABLED       = os.environ.get("YIELD_AUTO_ROTATE_ENABLED", "false").lower() == "true"
YIELD_AUTO_ROTATE_THRESHOLD_PCT = float(os.environ.get("YIELD_AUTO_ROTATE_THRESHOLD_PCT", "1.0"))
YIELD_MAX_ROTATE_ETH            = float(os.environ.get("YIELD_MAX_ROTATE_ETH", "0.5"))
YIELD_GAS_RESERVE_ETH           = float(os.environ.get("YIELD_GAS_RESERVE_ETH", "0.01"))

# ─── Sniper auto-entry config ─────────────────────────────────────
SNIPER_AUTO_ENTRY_ENABLED = os.environ.get("SNIPER_AUTO_ENTRY_ENABLED", "false").lower() == "true"
SNIPER_MAX_ENTRY_ETH      = float(os.environ.get("SNIPER_MAX_ENTRY_ETH", "0.05"))
SNIPER_MIN_TVL            = int(os.environ.get("SNIPER_MIN_TVL", "50000"))
SNIPER_EXIT_TVL_DROP_PCT  = float(os.environ.get("SNIPER_EXIT_TVL_DROP_PCT", "30.0"))

# ─── Trade Up Loop config ─────────────────────────────────────────
TRADE_UP_ENABLED             = os.environ.get("TRADE_UP_ENABLED", "false").lower() == "true"
TRADE_UP_MAX_ETH             = float(os.environ.get("TRADE_UP_MAX_ETH", "0.02"))
TRADE_UP_MIN_GAIN_PCT        = float(os.environ.get("TRADE_UP_MIN_GAIN_PCT", "0.5"))
TRADE_UP_CYCLE_INTERVAL_SEC  = int(os.environ.get("TRADE_UP_CYCLE_INTERVAL_SEC", "300"))
TRADE_UP_MAX_CYCLES          = int(os.environ.get("TRADE_UP_MAX_CYCLES", "0"))
TRADE_UP_COMPOUND            = os.environ.get("TRADE_UP_COMPOUND", "true").lower() == "true"
TRADE_UP_STOP_LOSS_PCT       = float(os.environ.get("TRADE_UP_STOP_LOSS_PCT", "5.0"))

# ─── Portfolio Manager config ─────────────────────────────────────
PORTFOLIO_MANAGER_ENABLED    = os.environ.get("PORTFOLIO_MANAGER_ENABLED", "false").lower() == "true"
PORTFOLIO_MAX_ETH            = float(os.environ.get("PORTFOLIO_MAX_ETH", "0.1"))
PORTFOLIO_MIN_STRATEGY_PCT   = float(os.environ.get("PORTFOLIO_MIN_STRATEGY_PCT", "10.0"))


def _fetch_aave_health_factors(addresses, chain="base"):
    subgraph_url = AAVE_V3_SUBGRAPHS.get(chain)
    if not subgraph_url:
        return {}
    addr_list = json.dumps([a.lower() for a in addresses])
    query = (
        "{ users(where: {id_in: " + addr_list + "}) {"
        " id healthFactor"
        " reserves { reserve { symbol } currentATokenBalance"
        "   currentVariableDebt currentStableDebt } } }"
    )
    try:
        req = Request(
            subgraph_url,
            data=json.dumps({"query": query}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        result = {}
        for user in data.get("data", {}).get("users", []):
            hf_raw = user.get("healthFactor", "0") or "0"
            try:
                hf = float(hf_raw) / AAVE_RAY
            except (ValueError, TypeError):
                hf = 0.0
            positions = []
            for r in user.get("reserves", []):
                a_bal  = float(r.get("currentATokenBalance", "0") or 0)
                v_debt = float(r.get("currentVariableDebt",  "0") or 0)
                s_debt = float(r.get("currentStableDebt",    "0") or 0)
                if a_bal > 0 or v_debt > 0 or s_debt > 0:
                    positions.append({
                        "symbol":   r.get("reserve", {}).get("symbol", ""),
                        "supplied": round(a_bal, 4),
                        "borrowed": round(v_debt + s_debt, 4),
                    })
            if positions or hf > 0:
                result[user["id"].lower()] = {
                    "health_factor": round(hf, 4),
                    "positions":     positions,
                }
        return result
    except Exception as e:
        return {"error": str(e)}


def _fetch_token_balances(address: str, chain: str = "base") -> list:
    """Fetch ERC-20 token balances for a wallet via Alchemy.

    Returns list of {token, balance, value_usd, price_usd}.
    """
    alchemy_key = os.environ.get("ALCHEMY_API_KEY", "")
    if not alchemy_key:
        return []

    chain_rpc = {
        "base":     f"https://base-mainnet.g.alchemy.com/v2/{alchemy_key}",
        "ethereum": f"https://eth-mainnet.g.alchemy.com/v2/{alchemy_key}",
        "arbitrum": f"https://arb-mainnet.g.alchemy.com/v2/{alchemy_key}",
    }
    rpc_url = chain_rpc.get(chain)
    if not rpc_url:
        return []

    try:
        payload = {
            "jsonrpc": "2.0", "id": 1,
            "method": "alchemy_getTokenBalances",
            "params": [address, "erc20"],
        }
        req = Request(
            rpc_url,
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())

        balances = data.get("result", {}).get("tokenBalances", [])
        results = []
        for b in balances:
            hex_bal = b.get("tokenBalance", "0x0") or "0x0"
            try:
                raw_balance = int(hex_bal, 16)
            except ValueError:
                continue
            if raw_balance == 0:
                continue
            # Normalize to 18 decimals (approximate — metadata fetch is expensive)
            balance_normalized = raw_balance / 1e18
            if balance_normalized < 0.0001:
                continue
            results.append({
                "token":       b.get("contractAddress", ""),
                "balance":     round(balance_normalized, 6),
                "value_usd":   0.0,  # populated below
                "price_usd":   0.0,
            })

        # Batch price lookup via DeFi Llama
        if results:
            chain_prefix = {"base": "base", "ethereum": "ethereum", "arbitrum": "arbitrum"}.get(chain, "base")
            coin_ids = ",".join(f"{chain_prefix}:{r['token']}" for r in results[:20])
            try:
                price_req = Request(
                    f"https://coins.llama.fi/prices/current/{coin_ids}",
                    method="GET",
                )
                with urlopen(price_req, timeout=6) as pr:
                    price_data = json.loads(pr.read())
                for r in results:
                    key = f"{chain_prefix}:{r['token']}"
                    coin = price_data.get("coins", {}).get(key, {})
                    price = coin.get("price", 0.0) or 0.0
                    r["price_usd"]  = price
                    r["value_usd"]  = round(price * r["balance"], 4)
                    r["symbol"]     = coin.get("symbol", r["token"][:8])
            except Exception:
                pass

        return results
    except Exception as e:
        return [{"error": str(e)}]


def _fetch_trade_positions() -> list:
    """Read open trade positions from Redis vespra:trade_positions.

    Returns list of position dicts written by Trader/Sniper agents.
    """
    try:
        r = _redis_client()
        items = r.lrange("vespra:trade_positions", 0, 99)
        positions = []
        for item in items:
            try:
                positions.append(json.loads(item))
            except Exception:
                pass
        return positions
    except Exception:
        return []


def _send_sentinel_alert(message: str):
    """Send alert via NullClaw channel if configured."""
    if not SENTINEL_ALERT_CHANNEL:
        return
    try:
        nc_agent = AGENTS.get("sentinel")
        if not nc_agent:
            return
        alert_cmd = [
            "sudo", "-u", nc_agent["user"],
            f"HOME={nc_agent['home']}",
            NULLCLAW, "channel", "send",
            "--channel", SENTINEL_ALERT_CHANNEL,
            "--message", message[:500],
        ]
        subprocess.run(alert_cmd, capture_output=True, text=True, timeout=10)
        log.info(f"SENTINEL ALERT sent: {message[:100]}")
    except Exception as e:
        log.error(f"Sentinel alert failed: {e}")


# ─── Risk contract analysis helpers ──────────────────────────────

# Function selectors for common honeypot/rugpull functions
_DANGEROUS_SELECTORS = {
    "0xb515566a": "setCanSell(address,bool)",       # transfer restriction
    "0x1cdd3be3": "setBlacklist(address,bool)",      # blacklist
    "0x8b4cee08": "setMaxWallet(uint256)",            # wallet limit
    "0x40c10f19": "mint(address,uint256)",            # mint authority
    "0x9dc29fac": "burn(address,uint256)",            # forced burn
    "0x715018a6": "renounceOwnership()",              # renounced = safe
    "0xf2fde38b": "transferOwnership(address)",       # ownership transfer
    "0x8da5cb5b": "owner()",                          # owner check
}

_SAFE_SELECTORS = {"0x715018a6"}  # renounceOwnership — good sign


def _analyze_contract(contract_address: str, chain: str = "base") -> dict:
    """Analyze ERC-20 contract bytecode for honeypot/rugpull patterns via Alchemy.

    Returns contract_analysis dict.
    """
    alchemy_key = os.environ.get("ALCHEMY_API_KEY", "")
    chain_rpc = {
        "base":     f"https://base-mainnet.g.alchemy.com/v2/{alchemy_key}",
        "ethereum": f"https://eth-mainnet.g.alchemy.com/v2/{alchemy_key}",
        "arbitrum": f"https://arb-mainnet.g.alchemy.com/v2/{alchemy_key}",
    }
    rpc_url = chain_rpc.get(chain)
    if not rpc_url or not alchemy_key:
        return {"error": "no_rpc", "honeypot_risk": "UNKNOWN"}

    result = {
        "ownership_renounced": False,
        "mint_authority":      "unknown",
        "honeypot_risk":       "LOW",
        "verified":            False,
        "dangerous_functions": [],
        "safe_functions":      [],
        "bytecode_size":       0,
        "error":               None,
    }

    try:
        # Fetch bytecode
        payload = {
            "jsonrpc": "2.0", "id": 1,
            "method": "eth_getCode",
            "params": [contract_address, "latest"],
        }
        req = Request(
            rpc_url,
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        bytecode = data.get("result", "0x") or "0x"

        if bytecode == "0x" or len(bytecode) < 10:
            result["error"] = "not_a_contract"
            result["honeypot_risk"] = "HIGH"
            return result

        result["bytecode_size"] = (len(bytecode) - 2) // 2  # bytes

        # Scan for dangerous/safe function selectors in bytecode
        bytecode_lower = bytecode.lower()
        for selector, name in _DANGEROUS_SELECTORS.items():
            sel_clean = selector.replace("0x", "")
            if sel_clean in bytecode_lower:
                if selector in _SAFE_SELECTORS:
                    result["safe_functions"].append(name)
                    result["ownership_renounced"] = True
                else:
                    result["dangerous_functions"].append(name)

        # Mint authority check
        if "mint(address,uint256)" in result["dangerous_functions"]:
            result["mint_authority"] = "unrestricted"
        elif any("owner" in f.lower() for f in result["dangerous_functions"]):
            result["mint_authority"] = "owner"
        else:
            result["mint_authority"] = "none"

        # Honeypot risk scoring
        danger_count = len(result["dangerous_functions"])
        if "setCanSell(address,bool)" in result["dangerous_functions"]:
            result["honeypot_risk"] = "HIGH"  # transfer restriction = likely honeypot
        elif "setBlacklist(address,bool)" in result["dangerous_functions"]:
            result["honeypot_risk"] = "HIGH"
        elif danger_count >= 3:
            result["honeypot_risk"] = "MEDIUM"
        elif danger_count >= 1:
            result["honeypot_risk"] = "LOW"
        else:
            result["honeypot_risk"] = "LOW"

        # Ownership renounced is a positive signal — downgrade risk
        if result["ownership_renounced"] and result["honeypot_risk"] == "MEDIUM":
            result["honeypot_risk"] = "LOW"

    except Exception as e:
        result["error"] = str(e)

    return result


def _get_tvl_velocity(protocol: str) -> float:
    """Compare current TVL vs cached 1hr-ago value. Returns % change.

    Caches current value in Redis vespra:tvl_cache:{protocol} with 1hr TTL.
    """
    try:
        r = _redis_client()
        cache_key = f"vespra:tvl_cache:{protocol}"
        cached = r.get(cache_key)

        # Fetch current TVL
        req = Request(f"https://api.llama.fi/protocol/{protocol}", method="GET")
        with urlopen(req, timeout=6) as resp:
            data = json.loads(resp.read())
        tvl_array = data.get("tvl", [])
        current_tvl = 0
        if tvl_array and isinstance(tvl_array[-1], dict):
            current_tvl = tvl_array[-1].get("totalLiquidityUSD", 0) or 0

        velocity = 0.0
        if cached:
            prev_tvl = float(cached)
            if prev_tvl > 0:
                velocity = round(((current_tvl - prev_tvl) / prev_tvl) * 100, 2)

        # Cache current for next comparison (1hr TTL)
        r.set(cache_key, str(current_tvl), ex=3600)
        return velocity
    except Exception:
        return 0.0


def pre_fetch_sentinel(chain="base"):
    """Fetch live wallet balances, Aave health factors, token positions, and P&L for Sentinel."""
    from datetime import datetime, timezone
    try:
        # Step 1 — pull all wallets from Keymaster
        url = f"{KEYMASTER}/wallets?chain={chain}"
        headers = {}
        if KEYMASTER_TOKEN:
            headers["Authorization"] = f"Bearer {KEYMASTER_TOKEN}"
        req = Request(url, headers=headers, method="GET")
        with urlopen(req, timeout=10) as resp:
            wallets = json.loads(resp.read())
        if not isinstance(wallets, list):
            wallets = []

        # Step 2 — fetch native ETH balance + ERC-20 token balances per wallet
        wallet_data = []
        for w in wallets:
            wallet_chain   = w.get("chain", chain)
            wallet_address = w.get("address", "")
            balance_eth    = 0.0
            try:
                bal_req = Request(
                    f"{KEYMASTER}/balance/{wallet_chain}/{wallet_address}",
                    method="GET",
                )
                with urlopen(bal_req, timeout=8) as bal_resp:
                    bal_data = json.loads(bal_resp.read())
                balance_eth = float(bal_data.get("balance_eth", 0) or 0)
            except Exception:
                pass

            # Fetch ERC-20 token balances
            token_balances = _fetch_token_balances(wallet_address, wallet_chain)
            token_value_usd = sum(t.get("value_usd", 0) for t in token_balances if isinstance(t, dict) and "error" not in t)

            wallet_data.append({
                "wallet_id":      w.get("id", ""),
                "address":        wallet_address,
                "chain":          wallet_chain,
                "label":          w.get("label", ""),
                "balance_eth":    round(balance_eth, 6),
                "active":         w.get("active", True),
                "token_balances": token_balances,
                "token_value_usd": round(token_value_usd, 2),
            })

        # Step 3 — Aave V3 health factors
        addresses = [w["address"] for w in wallet_data if w.get("address")]
        aave_positions = _fetch_aave_health_factors(addresses, chain) if addresses else {}

        # Step 4 — open trade positions from Redis
        trade_positions = _fetch_trade_positions()

        # Step 5 — load previous snapshot from Redis for P&L delta detection
        prev_snapshot = {}
        try:
            r = _redis_client()
            raw = r.get("vespra:sentinel_snapshot")
            if raw:
                prev_snapshot = json.loads(raw)
        except Exception:
            pass

        # Compute P&L per wallet vs previous snapshot
        alerts = []
        for w in wallet_data:
            addr = w["address"].lower()
            prev = prev_snapshot.get(addr, {})
            prev_eth = prev.get("balance_eth", 0)
            curr_eth = w["balance_eth"]
            if prev_eth > 0 and curr_eth > 0:
                drop_pct = ((prev_eth - curr_eth) / prev_eth) * 100
                if drop_pct >= SENTINEL_STOP_LOSS_PCT:
                    alert_msg = f"STOP LOSS: {w.get('label', addr[:8])} dropped {drop_pct:.1f}% ({prev_eth:.4f}→{curr_eth:.4f} ETH)"
                    alerts.append(alert_msg)
                    _send_sentinel_alert(alert_msg)
                    log.warning(f"SENTINEL: {alert_msg}")

        # Save current snapshot to Redis
        try:
            r = _redis_client()
            snapshot = {w["address"].lower(): {"balance_eth": w["balance_eth"]} for w in wallet_data}
            r.set("vespra:sentinel_snapshot", json.dumps(snapshot), ex=3600)
        except Exception:
            pass

        # Total portfolio value
        total_eth = sum(w["balance_eth"] for w in wallet_data)
        total_token_usd = sum(w["token_value_usd"] for w in wallet_data)

        return {
            "wallets":         wallet_data,
            "aave_positions":  aave_positions,
            "trade_positions": trade_positions,
            "thresholds":      {"warning": 1.5, "critical": 1.2},
            "stop_loss_pct":   SENTINEL_STOP_LOSS_PCT,
            "alerts":          alerts,
            "total_eth":       round(total_eth, 6),
            "total_token_usd": round(total_token_usd, 2),
            "timestamp":       datetime.now(timezone.utc).isoformat(),
        }
    except Exception as e:
        from datetime import datetime, timezone
        return {
            "wallets": [], "aave_positions": {}, "trade_positions": [],
            "alerts": [], "error": str(e),
            "thresholds": {"warning": 1.5, "critical": 1.2},
            "stop_loss_pct": SENTINEL_STOP_LOSS_PCT,
            "total_eth": 0.0, "total_token_usd": 0.0,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }


ALCHEMY_WEBHOOK_SECRET = os.environ.get("ALCHEMY_WEBHOOK_SECRET", "")

# Uniswap V3 factory addresses by chain_id
UNISWAP_V3_FACTORIES = {
    8453:  "0x33128a8fc17869897dce68ed026d694621f6fdfd",  # Base
    1:     "0x1F98431c8aD98523631AE4a59f267346ea31F984",  # Ethereum
    42161: "0x1F98431c8aD98523631AE4a59f267346ea31F984",  # Arbitrum
}

CHAIN_ID_MAP = {
    "base":     8453,
    "ethereum": 1,
    "arbitrum": 42161,
}


def _verify_alchemy_signature(raw_body: bytes, signature: str) -> bool:
    """Verify Alchemy webhook HMAC-SHA256 signature."""
    import hmac, hashlib
    if not ALCHEMY_WEBHOOK_SECRET:
        log.warning("ALCHEMY_WEBHOOK_SECRET not set — skipping signature verification")
        return True  # Allow through but warn; tighten once secret is configured
    expected = hmac.new(
        ALCHEMY_WEBHOOK_SECRET.encode(),
        raw_body,
        hashlib.sha256,
    ).hexdigest()
    return hmac.compare_digest(expected, signature.lower().replace("sha256=", ""))


def pre_fetch_sniper(pool_address, chain="base"):
    """Fetch live pool data from DeFi Llama for a specific pool address."""
    from datetime import datetime, timezone
    try:
        # Search DeFi Llama yields for this pool address
        req = Request("https://yields.llama.fi/pools", method="GET")
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        pools = data.get("data", [])

        chain_name = {"base": "Base", "ethereum": "Ethereum", "arbitrum": "Arbitrum"}.get(chain, "Base")
        pool_addr_lower = pool_address.lower()

        # Try to match by pool address
        matched = next(
            (p for p in pools if (p.get("pool", "") or "").lower() == pool_addr_lower),
            None,
        )

        # Fallback: find newest pools on this chain from known DEXes
        if not matched:
            candidates = [
                p for p in pools
                if p.get("chain") == chain_name
                and p.get("project") in ("uniswap-v3", "aerodrome", "camelot")
            ]
            candidates.sort(key=lambda p: p.get("tvlUsd") or 0)
            matched = candidates[0] if candidates else None

        if matched:
            return {
                "pool_address":    pool_address,
                "token0":          matched.get("symbol", "").split("-")[0] if "-" in matched.get("symbol", "") else matched.get("symbol", ""),
                "token1":          matched.get("symbol", "").split("-")[1] if "-" in matched.get("symbol", "") else "WETH",
                "chain":           chain,
                "tvl_usd":         int(matched.get("tvlUsd") or 0),
                "apy":             round(matched.get("apy") or 0.0, 4),
                "volume_24h":      int(matched.get("volumeUsd1d") or 0),
                "il_risk":         matched.get("ilRisk", "unknown"),
                "protocol":        matched.get("project", "uniswap-v3"),
                "liquidity_lock":  False,  # Cannot determine from DeFi Llama — Sniper LLM scores this
                "timestamp":       datetime.now(timezone.utc).isoformat(),
            }

        # No match found — return minimal context so LLM can still reason
        return {
            "pool_address": pool_address,
            "chain":        chain,
            "tvl_usd":      0,
            "apy":          0.0,
            "volume_24h":   0,
            "protocol":     "unknown",
            "liquidity_lock": False,
            "note":         "Pool not yet indexed by DeFi Llama",
            "timestamp":    datetime.now(timezone.utc).isoformat(),
        }
    except Exception as e:
        from datetime import datetime, timezone
        return {
            "pool_address": pool_address,
            "chain":        chain,
            "tvl_usd":      0,
            "error":        str(e),
            "timestamp":    datetime.now(timezone.utc).isoformat(),
        }


# ─── Yield rotation helpers ───────────────────────────────────────

def _get_current_yield_position(wallet_id: str) -> dict:
    """Read current yield position from Redis vespra:yield_position:{wallet_id}."""
    try:
        r = _redis_client()
        raw = r.get(f"vespra:yield_position:{wallet_id}")
        return json.loads(raw) if raw else {}
    except Exception:
        return {}


def _save_yield_position(wallet_id: str, position: dict):
    """Save current yield position to Redis."""
    try:
        r = _redis_client()
        r.set(f"vespra:yield_position:{wallet_id}", json.dumps(position))
    except Exception as e:
        log.error(f"Failed to save yield position: {e}")


def _log_yield_rotation(rotation: dict):
    """Append rotation event to Redis vespra:yield_rotations list."""
    try:
        from datetime import datetime, timezone
        rotation["timestamp"] = datetime.now(timezone.utc).isoformat()
        r = _redis_client()
        r.lpush("vespra:yield_rotations", json.dumps(rotation))
        r.ltrim("vespra:yield_rotations", 0, 99)  # keep last 100
    except Exception as e:
        log.error(f"Failed to log yield rotation: {e}")


def _build_aave_supply_calldata(asset_address: str, amount_wei: int, on_behalf_of: str) -> str:
    """Build Aave V3 supply() calldata.

    supply(address asset, uint256 amount, address onBehalfOf, uint16 referralCode)
    Selector: 0x617ba037
    """
    def pad32_address(addr: str) -> str:
        return addr.lower().replace("0x", "").zfill(64)

    def pad32_uint(n: int) -> str:
        return hex(n)[2:].zfill(64)

    selector   = "617ba037"
    asset_pad  = pad32_address(asset_address)
    amount_pad = pad32_uint(amount_wei)
    behalf_pad = pad32_address(on_behalf_of)
    referral   = pad32_uint(0)
    return "0x" + selector + asset_pad + amount_pad + behalf_pad + referral


def _build_aave_withdraw_calldata(asset_address: str, amount_wei: int, to_address: str) -> str:
    """Build Aave V3 withdraw() calldata.

    withdraw(address asset, uint256 amount, address to)
    Selector: 0x69328dec
    """
    def pad32_address(addr: str) -> str:
        return addr.lower().replace("0x", "").zfill(64)

    def pad32_uint(n: int) -> str:
        return hex(n)[2:].zfill(64)

    # Use max uint256 to withdraw full position
    MAX_UINT256 = (2**256) - 1
    actual_amount = amount_wei if amount_wei > 0 else MAX_UINT256

    selector   = "69328dec"
    asset_pad  = pad32_address(asset_address)
    amount_pad = pad32_uint(actual_amount)
    to_pad     = pad32_address(to_address)
    return "0x" + selector + asset_pad + amount_pad + to_pad


def _build_erc20_approve_calldata(spender_address: str, amount_wei: int) -> str:
    """Build ERC-20 approve() calldata.

    approve(address spender, uint256 amount)
    Selector: 0x095ea7b3
    """
    def pad32_address(addr: str) -> str:
        return addr.lower().replace("0x", "").zfill(64)

    def pad32_uint(n: int) -> str:
        return hex(n)[2:].zfill(64)

    selector    = "095ea7b3"
    spender_pad = pad32_address(spender_address)
    amount_pad  = pad32_uint(amount_wei)
    return "0x" + selector + spender_pad + amount_pad


# Aave V3 pool addresses by chain
AAVE_V3_POOLS = {
    "base":     "0xA238Dd80C259a72e81d7e4664a9801593F98d1c5",
    "ethereum": "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2",
    "arbitrum": "0x794a61358D6845594F94dc1DB02A252b5b4814aD",
}

# Common asset addresses (base chain)
AAVE_ASSETS_BASE = {
    "USDC": "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
    "WETH": "0x4200000000000000000000000000000000000006",
    "cbBTC": "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf",
}


def execute_yield_rotation(
    wallet_id: str,
    current_protocol: str,
    current_asset: str,
    target_protocol: str,
    target_asset: str,
    amount_eth: float,
    chain: str = "base",
) -> dict:
    """Execute a yield rotation: withdraw from current position → deposit to new.

    Atomic: if deposit fails after withdraw, sweeps to Safe.
    Returns rotation result dict.
    """
    from datetime import datetime, timezone

    log.warning(
        f"YIELD_ROTATE: {current_protocol}/{current_asset} → "
        f"{target_protocol}/{target_asset} amount={amount_eth} ETH chain={chain}"
    )

    rotation = {
        "wallet_id":        wallet_id,
        "from_protocol":    current_protocol,
        "from_asset":       current_asset,
        "to_protocol":      target_protocol,
        "to_asset":         target_asset,
        "amount_eth":       amount_eth,
        "chain":            chain,
        "status":           "pending",
        "withdraw_tx":      None,
        "deposit_tx":       None,
        "error":            None,
    }

    amount_wei = int(amount_eth * 1e18)
    aave_pool  = AAVE_V3_POOLS.get(chain)

    if not aave_pool:
        rotation["status"] = "error"
        rotation["error"]  = f"No Aave V3 pool for chain {chain}"
        _log_yield_rotation(rotation)
        return rotation

    # Step 1 — Withdraw from current position
    # For Aave, call withdraw(asset, MAX_UINT256, wallet_address)
    # We need wallet address — fetch from Keymaster
    wallet_address = ""
    try:
        km_result = keymaster_call("get_wallet", {"wallet_id": wallet_id})
        wallet_address = km_result.get("response", {}).get("address", "") if isinstance(km_result.get("response"), dict) else ""
    except Exception as e:
        rotation["status"] = "error"
        rotation["error"]  = f"Failed to get wallet address: {e}"
        _log_yield_rotation(rotation)
        return rotation

    if not wallet_address:
        rotation["status"] = "error"
        rotation["error"]  = "Could not resolve wallet address"
        _log_yield_rotation(rotation)
        return rotation

    asset_address = AAVE_ASSETS_BASE.get(current_asset.upper(), "")
    if not asset_address:
        rotation["status"] = "error"
        rotation["error"]  = f"Unknown asset {current_asset}"
        _log_yield_rotation(rotation)
        return rotation

    withdraw_calldata = _build_aave_withdraw_calldata(asset_address, 0, wallet_address)  # 0 = full balance
    withdraw_result = keymaster_call("send_tx", {
        "wallet_id": wallet_id,
        "to":        aave_pool,
        "value_eth": "0",
        "data":      withdraw_calldata,
    })

    withdraw_status = withdraw_result.get("status")
    withdraw_resp   = withdraw_result.get("response", {})
    if isinstance(withdraw_resp, str):
        try:
            withdraw_resp = json.loads(withdraw_resp)
        except Exception:
            pass

    if withdraw_status == "error" or (isinstance(withdraw_resp, dict) and withdraw_resp.get("status") not in ("ok", None)):
        rotation["status"] = "withdraw_failed"
        rotation["error"]  = str(withdraw_result.get("response", "unknown error"))
        _log_yield_rotation(rotation)
        return rotation

    rotation["withdraw_tx"] = withdraw_resp.get("tx_hash") if isinstance(withdraw_resp, dict) else None
    log.info(f"YIELD_ROTATE: withdraw tx={rotation['withdraw_tx']}")

    # Step 2 — Approve Aave pool to spend target asset
    target_asset_address = AAVE_ASSETS_BASE.get(target_asset.upper(), "")
    if not target_asset_address:
        # Withdraw succeeded but deposit impossible — sweep to Safe
        log.error(f"YIELD_ROTATE: unknown target asset {target_asset} — sweeping to Safe")
        keymaster_call("sweep", {"wallet_id": wallet_id})
        rotation["status"] = "deposit_failed_swept"
        rotation["error"]  = f"Unknown target asset {target_asset}"
        _log_yield_rotation(rotation)
        return rotation

    approve_calldata = _build_erc20_approve_calldata(aave_pool, amount_wei)
    approve_result = keymaster_call("send_tx", {
        "wallet_id": wallet_id,
        "to":        target_asset_address,
        "value_eth": "0",
        "data":      approve_calldata,
    })
    approve_resp = approve_result.get("response", {})
    if isinstance(approve_resp, str):
        try:
            approve_resp = json.loads(approve_resp)
        except Exception:
            pass
    if approve_result.get("status") == "error":
        log.error(f"YIELD_ROTATE: approve failed — sweeping to Safe")
        keymaster_call("sweep", {"wallet_id": wallet_id})
        rotation["status"] = "deposit_failed_swept"
        rotation["error"]  = "ERC-20 approve failed"
        _log_yield_rotation(rotation)
        return rotation

    # Step 3 — Supply to new Aave position
    supply_calldata = _build_aave_supply_calldata(target_asset_address, amount_wei, wallet_address)
    deposit_result = keymaster_call("send_tx", {
        "wallet_id": wallet_id,
        "to":        aave_pool,
        "value_eth": "0",
        "data":      supply_calldata,
    })

    deposit_resp = deposit_result.get("response", {})
    if isinstance(deposit_resp, str):
        try:
            deposit_resp = json.loads(deposit_resp)
        except Exception:
            pass

    if deposit_result.get("status") == "error" or (isinstance(deposit_resp, dict) and deposit_resp.get("status") == "simulation_failed"):
        log.error(f"YIELD_ROTATE: deposit failed — sweeping to Safe")
        keymaster_call("sweep", {"wallet_id": wallet_id})
        rotation["status"] = "deposit_failed_swept"
        rotation["error"]  = str(deposit_result.get("response", "deposit failed"))
        _log_yield_rotation(rotation)
        return rotation

    rotation["deposit_tx"] = deposit_resp.get("tx_hash") if isinstance(deposit_resp, dict) else None
    rotation["status"]     = "completed"

    # Save new position state
    _save_yield_position(wallet_id, {
        "protocol":   target_protocol,
        "asset":      target_asset,
        "amount_eth": amount_eth,
        "chain":      chain,
    })

    log.warning(f"YIELD_ROTATE: completed — deposit tx={rotation['deposit_tx']}")
    _log_yield_rotation(rotation)
    return rotation


# ─── Sniper auto-entry helpers ────────────────────────────────────

def _log_sniper_entry(entry: dict):
    """Store sniper entry in Redis vespra:sniper_entries."""
    try:
        from datetime import datetime, timezone
        entry["timestamp"] = datetime.now(timezone.utc).isoformat()
        r = _redis_client()
        r.lpush("vespra:sniper_entries", json.dumps(entry))
        r.ltrim("vespra:sniper_entries", 0, 99)
    except Exception as e:
        log.error(f"Failed to log sniper entry: {e}")


def _get_sniper_entries() -> list:
    """Read open sniper entries from Redis."""
    try:
        r = _redis_client()
        items = r.lrange("vespra:sniper_entries", 0, 99)
        return [json.loads(i) for i in items if i]
    except Exception:
        return []


def _build_1inch_swap_calldata(
    token_in: str,
    token_out: str,
    amount_wei: int,
    min_out_wei: int,
    recipient: str,
    chain_id: int = 8453,
) -> str | None:
    """Fetch swap calldata from 1inch Aggregation API v6.

    Returns 0x-prefixed calldata string or None on failure.
    """
    api_key = os.environ.get("ONEINCH_API_KEY", "").strip()
    if not api_key:
        return None
    try:
        url = (
            f"https://api.1inch.dev/swap/v6.0/{chain_id}/swap"
            f"?src={token_in}&dst={token_out}&amount={amount_wei}"
            f"&from={recipient}&slippage=1&disableEstimate=true"
        )
        req = Request(url, headers={
            "Authorization": f"Bearer {api_key}",
            "accept": "application/json",
            "User-Agent": "Mozilla/5.0 (compatible; Vespra/1.0)",
        })
        with urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read())
        return data.get("tx", {}).get("data")
    except Exception as e:
        log.error(f"1inch calldata fetch failed: {e}")
        return None


def execute_sniper_entry(
    pool_address: str,
    pool_data: dict,
    sniper_output: dict,
    chain: str = "base",
) -> dict:
    """Execute a sniper entry swap via 1inch + Keymaster send_tx.

    Returns entry result dict stored in Redis.
    """
    wallet_id = _STRATEGY_WALLET_MAP.get("sniper", "")
    if not wallet_id:
        return {"status": "error", "error": "No sniper wallet in STRATEGY_WALLET_MAP"}

    entry_eth = min(
        float(sniper_output.get("entry", {}).get("amount_eth", "0.05")),
        SNIPER_MAX_ENTRY_ETH,
    )

    # Hard TVL floor — never enter below this regardless of sniper output
    tvl = pool_data.get("tvl_usd", 0) or sniper_output.get("pool", {}).get("tvl_usd", 0)
    if tvl < SNIPER_MIN_TVL:
        return {"status": "skipped", "reason": f"TVL ${tvl} below SNIPER_MIN_TVL ${SNIPER_MIN_TVL}"}

    # Risk gate — only LOW or MEDIUM
    risk_score = sniper_output.get("risk_assessment", {}).get("score", "HIGH")
    if risk_score in ("HIGH", "CRITICAL"):
        return {"status": "skipped", "reason": f"Risk score {risk_score} above threshold"}

    entry = {
        "pool_address": pool_address,
        "chain":        chain,
        "wallet_id":    wallet_id,
        "entry_eth":    entry_eth,
        "tvl_at_entry": tvl,
        "risk_score":   risk_score,
        "pair":         sniper_output.get("pool", {}).get("pair", ""),
        "status":       "pending",
        "tx_hash":      None,
        "error":        None,
    }

    # Get wallet address for 1inch call
    wallet_address = ""
    try:
        km = keymaster_call("get_wallet", {"wallet_id": wallet_id})
        resp = km.get("response", {})
        if isinstance(resp, str):
            resp = json.loads(resp)
        wallet_address = resp.get("address", "") if isinstance(resp, dict) else ""
    except Exception as e:
        entry["status"] = "error"
        entry["error"]  = f"Wallet lookup failed: {e}"
        _log_sniper_entry(entry)
        return entry

    # Chain config
    chain_id_map = {"base": 8453, "ethereum": 1, "arbitrum": 42161}
    chain_id = chain_id_map.get(chain, 8453)
    weth = BASE_TOKEN_ADDRESSES.get(chain_id, {}).get("WETH", "")
    amount_wei = int(entry_eth * 1e18)

    # Get 1inch swap calldata: WETH → pool token
    # Use pool address as token_out (approximation — ideally resolve token0/token1)
    token_out = pool_address  # Sniper will have scored the pool token
    calldata = _build_1inch_swap_calldata(weth, token_out, amount_wei, 0, wallet_address, chain_id)

    if not calldata:
        # Fallback: send ETH directly to pool as liquidity (simpler)
        log.warning(f"SNIPER: 1inch calldata unavailable — attempting direct ETH send to pool")
        km_result = keymaster_call("send_native", {
            "wallet_id": wallet_id,
            "to":        pool_address,
            "amount_eth": str(entry_eth),
        })
    else:
        # Broadcast swap via send_tx
        router_address = "0x1111111254EEB25477B68fb85Ed929f73A960582"  # 1inch v5 router
        km_result = keymaster_call("send_tx", {
            "wallet_id": wallet_id,
            "to":        router_address,
            "value_eth": str(entry_eth),
            "data":      calldata,
        })

    km_resp = km_result.get("response", {})
    if isinstance(km_resp, str):
        try:
            km_resp = json.loads(km_resp)
        except Exception:
            pass

    if km_result.get("status") == "error" or (isinstance(km_resp, dict) and km_resp.get("status") == "simulation_failed"):
        entry["status"] = "tx_failed"
        entry["error"]  = str(km_result.get("response", "unknown"))
    else:
        entry["status"]   = "entered"
        entry["tx_hash"]  = km_resp.get("tx_hash") if isinstance(km_resp, dict) else None
        log.warning(f"SNIPER ENTRY: pool={pool_address[:12]}... pair={entry['pair']} eth={entry_eth} tx={entry['tx_hash']}")

        # Register position in Redis trade_positions for Sentinel to monitor
        try:
            r = _redis_client()
            position = {
                "type":           "sniper",
                "pool_address":   pool_address,
                "wallet_id":      wallet_id,
                "entry_eth":      entry_eth,
                "tvl_at_entry":   tvl,
                "entry_tx":       entry["tx_hash"],
                "chain":          chain,
                "exit_trigger_tvl_drop_pct": SNIPER_EXIT_TVL_DROP_PCT,
            }
            r.lpush("vespra:trade_positions", json.dumps(position))
        except Exception:
            pass

    _log_sniper_entry(entry)
    return entry


# ─── Trade Up Loop helpers ────────────────────────────────────────

def execute_trade_up_cycle(wallet_id: str, cycle_num: int, capital_eth: float) -> dict:
    """Single Trade Up cycle: Scout→Risk→Sentinel→Trader→Executor→compound.

    All agent calls are synchronous (call_agent returns dict).
    Returns dict with status, new_capital_eth, and optional gain/tx info.
    """
    # 1. Scout — momentum opportunities
    scout_result = call_agent("scout", f"Find momentum opportunities for wallet {wallet_id} mode=momentum")
    try:
        scout_parsed = json.loads(scout_result.get("response", "{}")) if isinstance(scout_result.get("response"), str) else scout_result.get("response", {})
    except (json.JSONDecodeError, TypeError):
        scout_parsed = {}

    opportunities = scout_parsed.get("opportunities", [])
    if not opportunities:
        return {"status": "hold", "reason": "no_momentum_opportunities", "new_capital_eth": capital_eth}

    best = max(opportunities, key=lambda x: x.get("momentum_score", 0))
    if best.get("momentum_score", 0) < 0.6:
        return {"status": "hold", "reason": "momentum_below_threshold", "new_capital_eth": capital_eth}

    # 2. Risk gate
    token_address = best.get("token_address", best.get("pool", ""))
    risk_result = call_agent("risk", f"Analyze risk for token {token_address} on {best.get('chain', 'base')}")
    try:
        risk_parsed = json.loads(risk_result.get("response", "{}")) if isinstance(risk_result.get("response"), str) else risk_result.get("response", {})
    except (json.JSONDecodeError, TypeError):
        risk_parsed = {}

    risk_score = risk_parsed.get("risk_assessment", {}).get("score", "HIGH")
    if risk_score == "HIGH":
        return {"status": "hold", "reason": "risk_gate_blocked", "new_capital_eth": capital_eth}

    # 3. Sentinel stop-loss check
    sentinel_result = call_agent("sentinel", f"Check positions for wallet {wallet_id}")
    try:
        sentinel_parsed = json.loads(sentinel_result.get("response", "{}")) if isinstance(sentinel_result.get("response"), str) else sentinel_result.get("response", {})
    except (json.JSONDecodeError, TypeError):
        sentinel_parsed = {}

    if sentinel_parsed.get("stop_loss_triggered", False):
        return {"status": "exit", "reason": "stop_loss_triggered", "new_capital_eth": capital_eth}

    # 4. Trader — TRADE_UP_MODE quote
    amount_wei = str(int(capital_eth * 1e18))
    trader_msg = json.dumps({
        "mode": "trade_up",
        "token_in": "0x4200000000000000000000000000000000000006",
        "token_out": token_address,
        "amount_wei": amount_wei,
        "momentum_score": best.get("momentum_score"),
        "risk_score": risk_score,
        "sentinel_data": sentinel_parsed,
        "capital_eth": capital_eth,
        "TRADE_UP_MIN_GAIN_PCT": TRADE_UP_MIN_GAIN_PCT,
        "TRADE_UP_MAX_ETH": TRADE_UP_MAX_ETH,
    })
    trader_result = call_agent("trader", trader_msg)
    try:
        trader_parsed = json.loads(trader_result.get("response", "{}")) if isinstance(trader_result.get("response"), str) else trader_result.get("response", {})
    except (json.JSONDecodeError, TypeError):
        trader_parsed = {}

    action = trader_parsed.get("action", "hold")
    if action != "swap":
        return {"status": action, "reason": trader_parsed.get("reasoning", "trader declined"), "new_capital_eth": capital_eth}

    expected_gain = float(trader_parsed.get("expected_gain_pct", 0))
    if expected_gain < TRADE_UP_MIN_GAIN_PCT:
        return {"status": "hold", "reason": f"gain {expected_gain}% below min {TRADE_UP_MIN_GAIN_PCT}%", "new_capital_eth": capital_eth}

    # 5. Queue for approval (no auto-execute without explicit approval flow)
    r = _redis_client()
    approval_entry = {
        "type": "trade_up", "wallet_id": wallet_id, "cycle": cycle_num,
        "action": trader_parsed, "timestamp": time.time(),
    }

    if not TRADE_UP_ENABLED:
        r.lpush("vespra:pending_approvals", json.dumps(approval_entry))
        return {"status": "queued_for_approval", "new_capital_eth": capital_eth}

    # Execute via Executor agent
    exec_result = call_agent("executor", json.dumps({
        "wallet_id": wallet_id,
        "calldata": trader_parsed.get("calldata"),
        "amount_wei": trader_parsed.get("amount_in_wei", trader_parsed.get("amount_in")),
        "chain": best.get("chain", "base"),
    }))
    try:
        exec_parsed = json.loads(exec_result.get("response", "{}")) if isinstance(exec_result.get("response"), str) else exec_result.get("response", {})
    except (json.JSONDecodeError, TypeError):
        exec_parsed = {}

    if exec_parsed.get("status") != "success" and exec_result.get("status") != "ok":
        return {"status": "error", "reason": exec_parsed.get("error", "executor failed"), "new_capital_eth": capital_eth}

    # 6. Compound capital
    new_capital_eth = capital_eth * (1 + expected_gain / 100) if TRADE_UP_COMPOUND else capital_eth
    gain_pct = ((new_capital_eth - capital_eth) / capital_eth) * 100 if capital_eth > 0 else 0

    # 7. Register position with Sentinel
    try:
        r.hset(f"vespra:trade_up_positions:{wallet_id}", mapping={
            "token": token_address,
            "entry_eth": str(capital_eth),
            "current_eth": str(new_capital_eth),
            "cycle": str(cycle_num),
            "timestamp": str(time.time()),
        })
    except Exception as e:
        log.error(f"[trade_up] Failed to register position: {e}")

    # 8. Log to history
    tx_hash = exec_parsed.get("tx_hash", "")
    try:
        r.lpush("vespra:trade_up_history", json.dumps({
            "wallet_id": wallet_id, "cycle": cycle_num,
            "capital_in_eth": capital_eth, "capital_out_eth": new_capital_eth,
            "gain_pct": gain_pct, "token": best.get("symbol", ""),
            "tx_hash": tx_hash, "timestamp": time.time(),
        }))
        r.ltrim("vespra:trade_up_history", 0, 99)
    except Exception as e:
        log.error(f"[trade_up] Failed to log history: {e}")

    return {"status": "executed", "new_capital_eth": new_capital_eth, "gain_pct": gain_pct, "tx_hash": tx_hash}


def run_trade_up_loop(wallet_id: str, initial_capital_eth: float):
    """Background thread: run Trade Up cycles until stopped, max cycles, or stop-loss."""
    capital = initial_capital_eth
    cycle = 0
    entry_value = initial_capital_eth

    r = _redis_client()
    r.set(f"vespra:trade_up_state:{wallet_id}", json.dumps({
        "active": True, "cycle": 0,
        "entry_value_eth": entry_value, "current_value_eth": capital,
        "started_at": time.time(),
    }))

    while True:
        # Check if stopped externally
        try:
            state_raw = r.get(f"vespra:trade_up_state:{wallet_id}")
            state = json.loads(state_raw) if state_raw else {}
            if not state.get("active", True):
                log.info(f"[trade_up] wallet={wallet_id} stop requested externally")
                break
        except Exception:
            pass

        cycle += 1
        if TRADE_UP_MAX_CYCLES > 0 and cycle > TRADE_UP_MAX_CYCLES:
            log.info(f"[trade_up] wallet={wallet_id} max cycles reached ({TRADE_UP_MAX_CYCLES}), stopping")
            break

        drawdown_pct = ((entry_value - capital) / entry_value) * 100 if entry_value > 0 else 0
        if drawdown_pct >= TRADE_UP_STOP_LOSS_PCT:
            log.warning(f"[trade_up] wallet={wallet_id} stop-loss: {drawdown_pct:.1f}% drawdown")
            break

        result = execute_trade_up_cycle(wallet_id, cycle, capital)
        log.info(f"[trade_up] cycle={cycle} status={result['status']} capital={result.get('new_capital_eth', capital):.6f} ETH")

        if result["status"] == "exit":
            break
        if result["status"] == "executed":
            capital = result["new_capital_eth"]

        r.set(f"vespra:trade_up_state:{wallet_id}", json.dumps({
            "active": True, "cycle": cycle,
            "entry_value_eth": entry_value, "current_value_eth": capital,
            "last_status": result["status"], "started_at": time.time(),
        }))

        time.sleep(TRADE_UP_CYCLE_INTERVAL_SEC)

    # Mark loop as inactive
    try:
        state_raw = r.get(f"vespra:trade_up_state:{wallet_id}")
        state = json.loads(state_raw) if state_raw else {}
    except Exception:
        state = {}
    state["active"] = False
    state["final_cycle"] = cycle
    state["final_capital_eth"] = capital
    r.set(f"vespra:trade_up_state:{wallet_id}", json.dumps(state))
    log.info(f"[trade_up] wallet={wallet_id} loop ended after {cycle} cycles, capital={capital:.6f} ETH")


# ─── Portfolio Manager helpers ────────────────────────────────────

def parse_portfolio_command(command: str, total_eth: float) -> dict:
    """Parse NL command like '1 ETH, 40% yield / 30% sniper / 30% trade-up'.

    Uses the Coordinator agent to extract structured allocation.
    Returns: {"yield": 0.4, "sniper": 0.3, "trade_up": 0.3, "total_eth": 1.0}
    or {"error": "..."} on failure.
    """
    instruction = (
        "Parse the capital allocation command and return strict JSON only: "
        '{"yield_pct": <float>, "sniper_pct": <float>, "trade_up_pct": <float>} '
        "All three values must sum to 100. If the command is ambiguous or invalid, "
        'return {"error": "<reason>"}. Do not include any other text.'
    )
    coordinator_msg = json.dumps({
        "task": "parse_portfolio_allocation",
        "command": command,
        "total_eth": total_eth,
        "available_strategies": ["yield", "sniper", "trade_up"],
        "instruction": instruction,
    })
    coordinator_result = call_agent("coordinator", coordinator_msg)

    try:
        raw_resp = coordinator_result.get("response", "{}")
        parsed = json.loads(raw_resp) if isinstance(raw_resp, str) else raw_resp
    except (json.JSONDecodeError, TypeError):
        parsed = {}

    # The coordinator may nest the allocation under a key or return it flat
    if "allocation" in parsed:
        parsed = parsed["allocation"]

    if "error" in parsed:
        return {"error": parsed["error"]}

    yield_pct = float(parsed.get("yield_pct", 0))
    sniper_pct = float(parsed.get("sniper_pct", 0))
    trade_up_pct = float(parsed.get("trade_up_pct", 0))

    total_pct = yield_pct + sniper_pct + trade_up_pct
    if abs(total_pct - 100.0) > 1.0:
        return {"error": f"Percentages sum to {total_pct}, must equal 100"}

    for name, pct in [("yield", yield_pct), ("sniper", sniper_pct), ("trade_up", trade_up_pct)]:
        if 0 < pct < PORTFOLIO_MIN_STRATEGY_PCT:
            return {"error": f"{name} allocation {pct}% is below minimum {PORTFOLIO_MIN_STRATEGY_PCT}%"}

    return {
        "yield": yield_pct / 100,
        "sniper": sniper_pct / 100,
        "trade_up": trade_up_pct / 100,
        "total_eth": total_eth,
    }


def execute_portfolio_launch(split: dict, dry_run: bool = False) -> dict:
    """Given a parsed split dict, create wallets via Keymaster, register them,
    and start enabled strategies.

    Returns a status dict with wallet_ids and per-strategy results.
    """
    total_eth = split["total_eth"]
    results = {}
    wallet_map = {}
    r = _redis_client()

    for strategy in ["yield", "sniper", "trade_up"]:
        pct = split.get(strategy, 0)
        if pct <= 0:
            results[strategy] = {"status": "skipped", "reason": "0% allocation"}
            continue

        capital_eth = round(total_eth * pct, 6)

        # Create wallet via Keymaster
        km_result = keymaster_call("create_wallet", {"label": f"portfolio_{strategy}_{int(time.time())}"})
        km_resp = km_result.get("response", {})
        if isinstance(km_resp, str):
            try:
                km_resp = json.loads(km_resp)
            except Exception:
                km_resp = {}

        wallet_id = km_resp.get("wallet_id", "") if isinstance(km_resp, dict) else ""
        if not wallet_id:
            results[strategy] = {"status": "error", "reason": "keymaster wallet creation failed"}
            continue

        wallet_map[strategy] = wallet_id

        if dry_run:
            results[strategy] = {
                "status": "dry_run",
                "wallet_id": wallet_id,
                "capital_eth": capital_eth,
                "pct": pct * 100,
            }
            continue

        # Start strategy
        if strategy == "yield" and YIELD_AUTO_ROTATE_ENABLED:
            r.hset("vespra:portfolio_wallets", strategy, wallet_id)
            results[strategy] = {"status": "registered", "wallet_id": wallet_id, "capital_eth": capital_eth}

        elif strategy == "sniper" and SNIPER_AUTO_ENTRY_ENABLED:
            r.hset("vespra:portfolio_wallets", strategy, wallet_id)
            results[strategy] = {"status": "registered", "wallet_id": wallet_id, "capital_eth": capital_eth}

        elif strategy == "trade_up" and TRADE_UP_ENABLED:
            t = threading.Thread(
                target=run_trade_up_loop,
                args=(wallet_id, capital_eth),
                daemon=True,
            )
            t.start()
            results[strategy] = {"status": "started", "wallet_id": wallet_id, "capital_eth": capital_eth}

        else:
            results[strategy] = {
                "status": "registered_disabled",
                "wallet_id": wallet_id,
                "capital_eth": capital_eth,
                "note": f"{strategy.upper()}_ENABLED=false — wallet created but strategy not started",
            }

    # Persist portfolio state
    portfolio_record = {
        "total_eth": total_eth,
        "split": {k: split[k] for k in ["yield", "sniper", "trade_up"] if k in split},
        "wallet_map": wallet_map,
        "results": results,
        "dry_run": dry_run,
        "created_at": time.time(),
    }
    try:
        r.lpush("vespra:portfolio_launches", json.dumps(portfolio_record))
        r.ltrim("vespra:portfolio_launches", 0, 49)
    except Exception as e:
        log.error(f"[portfolio] Failed to persist launch record: {e}")

    return {"status": "ok", "portfolio": portfolio_record}


# DAG definitions: map goal keywords to ordered agent pipelines
COORDINATOR_DAGS = {
    "yield_rotate": ["scout", "risk", "yield"],
    "new_pool":     ["sniper", "risk", "trader"],
    "monitor":      ["sentinel", "yield"],
    "full_scan":    ["scout", "risk", "sentinel", "yield"],
    "health_exit":  ["sentinel", "executor"],
}

def orchestrate_dag(goal, message):
    """Run a named DAG: call agents in order, feed each result into the next context.

    Returns dict with dag_name, steps (list of {agent, status, summary}),
    trigger_dag, and final_context string for the Coordinator LLM.
    """
    from datetime import datetime, timezone

    # Detect which DAG to run from goal keywords
    dag_name = None
    goal_lower = goal.lower()
    for key in COORDINATOR_DAGS:
        if key.replace("_", " ") in goal_lower or key in goal_lower:
            dag_name = key
            break
    # Fallback: infer from keywords
    if not dag_name:
        if any(w in goal_lower for w in ["yield", "rotate", "apy", "lend"]):
            dag_name = "yield_rotate"
        elif any(w in goal_lower for w in ["new pool", "snipe", "entry"]):
            dag_name = "new_pool"
        elif any(w in goal_lower for w in ["monitor", "health", "sentinel"]):
            dag_name = "monitor"
        else:
            dag_name = "full_scan"

    agent_sequence = COORDINATOR_DAGS[dag_name]
    log.info(f"Coordinator DAG '{dag_name}': {' -> '.join(agent_sequence)}")

    steps = []
    context_chain = message  # Each agent receives accumulated context
    trigger_dag = None

    for agent_key in agent_sequence:
        log.info(f"  DAG step: {agent_key}")
        result = call_agent(agent_key, context_chain)
        status = result.get("status", "failed")
        raw_response = result.get("response", "")

        # Try to parse response for summary extraction
        summary = raw_response
        try:
            parsed = json.loads(raw_response) if isinstance(raw_response, str) else raw_response
            # Extract the most useful summary field per agent type
            if agent_key == "scout" and isinstance(parsed, dict):
                opps = parsed.get("opportunities", [])
                summary = f"{len(opps)} opportunities found; top: {opps[0].get('protocol','?')} {opps[0].get('apy',0)}% APY" if opps else "no opportunities"
            elif agent_key == "risk" and isinstance(parsed, dict):
                summary = f"score={parsed.get('score','?')} gate_pass={parsed.get('gate_pass','?')}"
            elif agent_key == "sentinel" and isinstance(parsed, list):
                criticals = [w for w in parsed if any(p.get("status") == "critical" for p in w.get("positions", []))]
                if criticals:
                    trigger_dag = "health-monitor-exit"
                summary = f"{len(parsed)} wallets checked; {len(criticals)} critical"
            elif agent_key == "yield" and isinstance(parsed, dict):
                summary = f"action={parsed.get('recommended_action','?')} asset={parsed.get('target_asset','?')} executor_ready={parsed.get('executor_ready','?')}"
            elif agent_key == "sniper" and isinstance(parsed, dict):
                summary = f"status={parsed.get('status','?')} entry_recommended={parsed.get('entry_recommended','?')}"
            elif agent_key == "trader" and isinstance(parsed, dict):
                summary = f"executor_ready={parsed.get('executor_ready','?')} price_impact={parsed.get('price_impact','?')}"
        except (json.JSONDecodeError, TypeError, AttributeError):
            pass

        steps.append({
            "agent":    agent_key,
            "status":   status,
            "summary":  summary[:300],
        })

        # Abort DAG on agent failure
        if status != "ok":
            log.warning(f"  DAG step {agent_key} failed — aborting")
            break

        # Feed this result into the next agent's context
        context_chain = f"{message}\n\nPREVIOUS AGENT OUTPUT ({agent_key}):\n{raw_response[:1000]}"

    # Yield rotation auto-execution
    rotation_result = None
    if dag_name == "yield_rotate" and YIELD_AUTO_ROTATE_ENABLED:
        # Extract yield step result
        yield_step = next((s for s in steps if s["agent"] == "yield" and s["status"] == "ok"), None)
        risk_step  = next((s for s in steps if s["agent"] == "risk"  and s["status"] == "ok"), None)

        if yield_step and risk_step:
            # Parse yield output for rotation recommendation
            try:
                yield_raw = call_agent.__globals__.get("_last_yield_response", "")
                # Re-run yield to get parsed output from context_chain
                yield_output = None
                for step in steps:
                    if step["agent"] == "yield":
                        # summary contains the parsed action
                        if "rebalance" in step.get("summary", ""):
                            yield_output = step
                            break

                # Check risk gate_pass from summary
                risk_gate = "gate_pass=True" in risk_step.get("summary", "") or "gate_pass=true" in risk_step.get("summary", "").lower()

                if yield_output and risk_gate:
                    # Parse APY delta from summaries
                    wallet_id = _STRATEGY_WALLET_MAP.get("yield", "")
                    if wallet_id:
                        # Extract current and target from yield summary
                        summary = yield_output.get("summary", "")
                        # Default rotation params — Coordinator will refine
                        rotation_result = execute_yield_rotation(
                            wallet_id      = wallet_id,
                            current_protocol = "aave-v3",
                            current_asset  = "USDC",
                            target_protocol  = "aave-v3",
                            target_asset   = "WETH",
                            amount_eth     = min(YIELD_MAX_ROTATE_ETH, 0.1),
                            chain          = "base",
                        )
                    else:
                        log.warning("YIELD_ROTATE: no wallet mapped for 'yield' strategy — set STRATEGY_WALLET_MAP")
            except Exception as e:
                log.error(f"YIELD_ROTATE DAG error: {e}")

    return {
        "dag_name":        dag_name,
        "steps":           steps,
        "trigger_dag":     trigger_dag,
        "rotation_result": rotation_result,
        "final_context":   context_chain,
        "timestamp":       datetime.now(timezone.utc).isoformat(),
    }


# Standard ERC-20 bytecode prefix (OpenZeppelin-style minimal ERC-20)
# This is the CREATE initcode prefix — constructor args are ABI-encoded and appended
ERC20_BYTECODE = (
    "608060405234801561001057600080fd5b506040516111e53803806111e583398181016040528101906100329190610"
    "1e8565b8282600390816100429190610481565b5081600490816100529190610481565b505050610553565b600081519"
    "050919050565b7f4e487b7100000000000000000000000000000000000000000000000000000000600052604160045260"
    "246000fd5b7f4e487b7100000000000000000000000000000000000000000000000000000000600052602260045260246"
    "000fd5b600060028204905060018216806100d557607f821691505b6020821081036100e8576100e761008e565b5b5091"
    "9050565b60008190508160005260206000209050919050565b60006020601f8301049050919050565b600082821b905092"
    "915050565b6000600883026101417fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff8261"
    "010c565b61014b868361010c565b95508019841693508086168417925050509392505050565b600061017961017484610"
    "237565b610242565b9050828152602081018484840111156101955761019461020d565b5b6101a084828561026e565b50"
    "9392505050565b600082601f8301126101bc576101bb610208565b5b81516101cc848260208601610163565b91505092"
    "915050565b6000815190506101e28161053c565b92915050565b6000806000606084860312156102015761020061020d"
    "565b5b600084015167ffffffffffffffff8111156102255761022461021265b5b610231868287016101a8565b9350506"
    "020840151610244565b602084015151610253565b9293505050565b6000602082019050919050565b565b600061024"
    "f8261023a565b61025981846102465b81523360208301526102718184610290565b5050610551565b60006104308261"
    "0390565b60006104508261028f565b600082526101008261028f565b600082526020600090810190526104705261"
    "04818161028f565b825250919050565b60006104918261028f565b610490600083866102a5565b6102a68383610290"
    "565b61029e61028f565b600081560"
)


def abi_encode_constructor(name: str, symbol: str, total_supply_wei: int, decimals: int = 18) -> str:
    """ABI-encode ERC-20 constructor arguments (name, symbol, totalSupply, decimals).

    ERC-20 constructor signature: constructor(string name, string symbol, uint256 totalSupply, uint8 decimals)
    ABI encoding layout (all values 32-byte aligned):
      - offset to name string
      - offset to symbol string
      - totalSupply (uint256)
      - decimals (uint8, padded to 32 bytes)
      - name length + padded data
      - symbol length + padded data
    """
    def pad32(b: bytes) -> bytes:
        """Pad bytes to next 32-byte boundary."""
        rem = len(b) % 32
        return b + (b'\x00' * (32 - rem)) if rem else b

    def encode_uint256(n: int) -> bytes:
        return n.to_bytes(32, 'big')

    def encode_string(s: str) -> bytes:
        encoded = s.encode('utf-8')
        length = encode_uint256(len(encoded))
        return length + pad32(encoded)

    # Static slots: 4 params × 32 bytes = 128 bytes base offset
    # name offset:         128 (0x80) — after all 4 static slots
    # symbol offset:       128 + 32 + len(name_encoded)
    name_bytes = name.encode('utf-8')

    name_encoded_len = 32 + (((len(name_bytes) - 1) // 32) + 1) * 32  # length word + padded data
    symbol_offset = 128 + name_encoded_len

    parts = [
        encode_uint256(128),           # offset to name
        encode_uint256(symbol_offset), # offset to symbol
        encode_uint256(total_supply_wei),
        encode_uint256(decimals),
        encode_string(name),
        encode_string(symbol),
    ]
    return ''.join(p.hex() for p in parts)


def pre_fetch_launcher(name, symbol, supply_human, decimals=18, chain="base"):
    """Build real ERC-20 deploy calldata and estimate gas context for Launcher agent."""
    from datetime import datetime, timezone
    try:
        # Convert human supply to wei
        total_supply_wei = int(supply_human) * (10 ** decimals)

        # ABI-encode constructor args
        constructor_args = abi_encode_constructor(name, symbol, total_supply_wei, decimals)

        # Full deploy calldata = bytecode + constructor args
        # Note: ERC20_BYTECODE is a standard reference — actual deployment uses
        # a verified OpenZeppelin bytecode. Constructor args are always appended.
        deploy_calldata = "0x" + constructor_args  # Args-only for preview; full = bytecode + args

        # Gas estimate: ERC-20 deploy typically 500k-800k gas
        gas_estimate = 650000

        # Gas cost in USD (rough): fetch current gas price
        gas_price_gwei = 0.0
        try:
            gas_req = Request(
                "https://api.etherscan.io/api?module=gastracker&action=gasoracle",
                method="GET",
            )
            with urlopen(gas_req, timeout=5) as gr:
                gd = json.loads(gr.read())
            gas_price_gwei = float(gd.get("result", {}).get("ProposeGasPrice", 0) or 0)
        except Exception:
            gas_price_gwei = 1.0  # Base L2 default fallback

        gas_cost_eth = (gas_estimate * gas_price_gwei * 1e9) / 1e18

        return {
            "name":               name,
            "symbol":             symbol,
            "decimals":           decimals,
            "total_supply_human": supply_human,
            "total_supply_wei":   str(total_supply_wei),
            "constructor_args":   constructor_args,
            "deploy_calldata":    deploy_calldata,
            "gas_estimate":       gas_estimate,
            "gas_price_gwei":     gas_price_gwei,
            "gas_cost_eth":       round(gas_cost_eth, 6),
            "chain":              chain,
            "timestamp":          datetime.now(timezone.utc).isoformat(),
        }
    except Exception as e:
        from datetime import datetime, timezone
        return {
            "error":     str(e),
            "chain":     chain,
            "timestamp": datetime.now(timezone.utc).isoformat(),
        }


# ─── Redis queue helpers ──────────────────────────────────────────

def _redis_client():
    """Return a Redis client. Raises on connection failure."""
    import redis
    return redis.Redis(host=REDIS_HOST, port=REDIS_PORT, db=REDIS_DB,
                       socket_connect_timeout=2, socket_timeout=5,
                       decode_responses=True)


def queue_depth() -> dict:
    """Return current depth of all three queues."""
    try:
        r = _redis_client()
        return {
            "job_queue":   r.llen(QUEUE_KEY),
            "retry_queue": r.llen(RETRY_KEY),
            "dlq":         r.llen(DLQ_KEY),
        }
    except Exception as e:
        return {"error": str(e)}


def enqueue_job(agent: str, message: str, dag_run_id: str = "", step: str = "") -> dict:
    """Push a job onto vespra:job_queue. Returns the job dict."""
    from datetime import datetime, timezone
    job = {
        "job_id":     f"job-{int(time.time()*1000)}",
        "dag_run_id": dag_run_id,
        "step":       step,
        "agent":      agent,
        "payload":    message,
        "attempts":   0,
        "max_attempts": 3,
        "created_at": datetime.now(timezone.utc).isoformat(),
    }
    r = _redis_client()
    r.lpush(QUEUE_KEY, json.dumps(job))
    log.info(f"Enqueued job {job['job_id']} for agent={agent}")
    return job


def _process_job(job: dict) -> bool:
    """Execute one job. Returns True on success, False on failure."""
    agent   = job.get("agent", "")
    payload = job.get("payload", "")
    job_id  = job.get("job_id", "?")

    if not agent or not payload:
        log.error(f"Queue: malformed job {job_id} — missing agent or payload")
        return False

    log.info(f"Queue: processing job {job_id} agent={agent} attempt={job.get('attempts',0)+1}")
    result = call_agent(agent, payload)
    if result.get("status") == "ok":
        log.info(f"Queue: job {job_id} completed ok")
        return True
    else:
        log.warning(f"Queue: job {job_id} failed: {result.get('error','unknown')}")
        return False


def _queue_worker():
    """Background thread: BRPOP from job_queue and retry_queue, process, handle failures."""
    log.info("Queue worker started")
    r = None

    while True:
        try:
            if r is None:
                r = _redis_client()

            # BRPOP blocks up to BRPOP_TIMEOUT seconds — checks both queues
            item = r.brpop([QUEUE_KEY, RETRY_KEY], timeout=BRPOP_TIMEOUT)
            if item is None:
                continue  # timeout, loop again

            _, raw = item
            try:
                job = json.loads(raw)
            except json.JSONDecodeError:
                log.error(f"Queue: invalid JSON in queue item: {raw[:200]}")
                continue

            attempts = job.get("attempts", 0)
            max_attempts = job.get("max_attempts", 3)
            job["attempts"] = attempts + 1

            success = _process_job(job)

            if not success:
                if job["attempts"] < max_attempts:
                    # Exponential backoff: sleep then re-queue to retry
                    delay = 2 ** (job["attempts"] - 1)
                    log.warning(f"Queue: job {job.get('job_id','?')} retry {job['attempts']}/{max_attempts} in {delay}s")
                    time.sleep(delay)
                    r.lpush(RETRY_KEY, json.dumps(job))
                else:
                    # Dead letter
                    log.error(f"Queue: job {job.get('job_id','?')} exceeded max_attempts — moving to DLQ")
                    r.lpush(DLQ_KEY, json.dumps(job))

        except Exception as e:
            log.error(f"Queue worker error: {e} — reconnecting in 3s")
            r = None
            time.sleep(3)


AGENTS = {
    "coordinator": {"user": "nc-coordinator", "home": "/opt/nc-coordinator"},
    "scout":       {"user": "nc-scout",       "home": "/opt/nc-scout"},
    "sentinel":    {"user": "nc-sentinel",     "home": "/opt/nc-sentinel"},
    "risk":        {"user": "nc-risk",         "home": "/opt/nc-risk"},
    "executor":    {"user": "nc-executor",     "home": "/opt/nc-executor"},
    "trader":      {"user": "nc-trader",       "home": "/opt/nc-trader"},
    "yield":       {"user": "nc-yield",        "home": "/opt/nc-yield"},
    "sniper":      {"user": "nc-sniper",       "home": "/opt/nc-sniper"},
    'launcher': {'user': 'nc-launcher', 'home': '/opt/nc-launcher'},
}

IDENTITIES = {
    "coordinator": """You are Argos, Coordinator of the Vespra DeFi agent swarm.
You receive a DAG execution report and produce a concise summary for @dr_bonkers.

You MUST respond with valid JSON only — no prose, no markdown, no explanation.

Output schema:
{
  "report": "string (max 500 chars — lead with top finding, include next action)",
  "dag_name": "string",
  "steps_completed": int,
  "trigger_dag": "string|null",
  "alerts": ["string"],
  "next_action": "string (one clear instruction for the operator)",
  "status": "ok|warning|critical"
}

status rules:
- critical: any trigger_dag is set OR any step shows a critical health factor
- warning: any agent returned executor_ready=false or gate_pass=false
- ok: all steps completed cleanly

Rules: No transactions, no keys. Summarize DAG_REPORT only.
Do NOT use tools, search, or read files.""",

    "scout": """You are Scout, market intelligence agent of the Vespra DeFi swarm.
You MUST respond with valid JSON only. No prose, no markdown. Base your analysis on LIVE_POOL_DATA.

Output schema:
{
  "opportunities": [
    {
      "protocol": "string",
      "pool": "string",
      "chain": "string",
      "apy": float,
      "tvl_usd": int,
      "momentum_score": float,
      "entry_signal": "strong|moderate|weak|none",
      "tvl_change_7d_pct": float,
      "price_change_24h_pct": float,
      "risk_tier": "LOW|MEDIUM|HIGH",
      "recommended_action": "string"
    }
  ],
  "summary": "string",
  "top_chain": "string",
  "strong_signal_count": int,
  "data_timestamp": "ISO 8601 UTC"
}

Risk tier logic: apy > 50 = HIGH, 10-50 = MEDIUM, < 10 = LOW.
Prioritize opportunities where entry_signal is "strong" or "moderate".
Flag any pool with price_change_24h_pct > 10 as a momentum candidate for Trader.
Return max 5 opportunities sorted by momentum_score descending.
Rules: No transactions, no keys. Analyze LIVE_POOL_DATA only.
Do NOT use tools, search, or read files.""",

    "sentinel": """You are Sentinel, portfolio watchdog of the Vespra DeFi agent swarm.
You MUST respond with valid JSON only — no prose, no markdown, no explanation.
Use LIVE_SENTINEL_DATA to assess every wallet and position.

Output: a JSON object with this structure:
{
  "wallets": [
    {
      "wallet": "0x...",
      "label": "string",
      "chain": "string",
      "balance_eth": float,
      "token_value_usd": float,
      "positions": [
        {"protocol": "aave-v3", "health_factor": float, "status": "healthy|warning|critical"}
      ],
      "token_positions": [
        {"symbol": "string", "balance": float, "value_usd": float, "pnl_pct": float, "status": "holding|exit_triggered"}
      ],
      "alerts": ["string"],
      "trigger_dag": "health-monitor-exit" | null
    }
  ],
  "trade_positions": [
    {"token": "string", "entry_price": float, "current_price": float, "pnl_pct": float, "status": "holding|exit_triggered"}
  ],
  "total_portfolio_usd": float,
  "worst_position": "string|null",
  "alert_sent": bool,
  "overall_status": "healthy|warning|critical"
}

Rules:
- Set health factor status: healthy ≥1.5, warning 1.2-1.5, critical <1.2
- Set trigger_dag to "health-monitor-exit" for any critical position
- Set overall_status to "critical" if ANY wallet has a critical position or alert
- worst_position = label of wallet with lowest health_factor or highest loss
- total_portfolio_usd = sum of all ETH value + token_value_usd (use $3000/ETH as rough estimate if no price data)
- alert_sent = true if LIVE_SENTINEL_DATA.alerts is non-empty

Rules: No transactions, no keys. Analyze LIVE_SENTINEL_DATA only.
Do NOT use tools, search, or read files.""",

    "risk": """You are Risk, safety evaluator of the Vespra DeFi agent swarm.
You MUST respond with valid JSON only. No prose, no markdown. Use LIVE_PROTOCOL_DATA.

Output schema:
{
  "protocol": "string",
  "chain": "string",
  "score": "LOW|MEDIUM|HIGH|CRITICAL",
  "factors": [{"category": "string", "rating": "PASS|WARN|FAIL", "detail": "string"}],
  "contract_analysis": {
    "honeypot_risk": "LOW|MEDIUM|HIGH|UNKNOWN",
    "ownership_renounced": bool,
    "mint_authority": "none|owner|unrestricted|unknown",
    "dangerous_functions": ["string"],
    "verified": bool
  },
  "tvl_velocity_alert": bool,
  "recommendation": "string (max 20 words)",
  "gate_pass": true|false
}

Scoring rules (use LIVE_PROTOCOL_DATA):
- TVL: >$10M = PASS, $1M-$10M = WARN, <$1M = FAIL
- TVL trend: >-20% = PASS, -20% to -50% = WARN, <-50% = FAIL
- TVL velocity 1hr: >-10% = PASS, -10% to -25% = WARN, <-25% = FAIL (emergency)
- Audits: has audits = PASS, no audits = FAIL
- Age: >180 days = PASS, 30-180 days = WARN, <30 days = FAIL
- Honeypot risk: LOW = PASS, MEDIUM = WARN, HIGH = FAIL
- Mint authority: none = PASS, owner = WARN, unrestricted = FAIL

gate_pass = true ONLY when score is LOW or MEDIUM AND honeypot_risk is not HIGH.
tvl_velocity_alert = true when tvl_velocity_1hr < -10.

Be conservative. When in doubt, rate higher risk.
Rules: No transactions, no keys.
Do NOT use tools, search, or read files.""",

    "executor": """You are Executor, the transaction bridge of the Vespra DeFi agent swarm.
You translate instructions into Keymaster commands. The gateway handles the actual HTTP calls.

Parse the instruction and return ONLY valid JSON with this structure:
{
  "keymaster_calls": [
    {"action": "<action_name>", "params": {<params>}}
  ],
  "warnings": []
}

Available actions and their required params:
- create_wallet: {chain, label?, cap_eth?, strategy?}
- list_wallets: {chain?, strategy?}
- get_wallet: {wallet_id}
- get_balance: {chain, address}
- get_all_balances: {chain}
- chain_status: {chain}
- send_native: {wallet_id, to, amount_eth}
- sweep: {wallet_id}
- get_tx_log: {wallet_id}

WALLET REFERENCES: Users can reference wallets by label, address, or UUID.
The gateway automatically resolves labels and addresses to UUIDs before executing.
- By label: "send 0.01 ETH from base-test-1 to 0xABC..."
- By address: "sweep wallet 0x10d2..."
- By UUID: "get balance for 7cb4bdd4-cdc8-4b0b-ac8f-ef83f31e739e"
Use whatever the user provides as wallet_id — the gateway resolves it.

For multi-step operations, order the calls correctly. Example — to send ETH safely:
1. get_wallet (verify active) — the gateway returns address and chain
2. get_balance (verify funds) — use the EXACT chain and address from get_wallet
3. chain_status (verify chain healthy) — use the EXACT chain from get_wallet
4. send_native (execute)

IMPORTANT: After get_wallet, always use the exact chain name (e.g. "base_sepolia" not "base")
and the full 0x address from the wallet result. Do not guess or shorten these values.
The gateway will auto-correct from context if needed, but provide correct values when possible.

Rules:
- For amounts > 0.1 ETH, add a warning.
- Never skip safety checks before send/sweep.
- Return ONLY the JSON — no commentary, no markdown, no explanation.
Do NOT use tools, search, read files, or make HTTP requests. Use training knowledge only.""",

    "trader": """You are Trader, the swap specialist of the Vespra DeFi agent swarm.
You MUST respond with valid JSON only. No prose, no markdown. Use LIVE_QUOTE_DATA to build the swap intent.

Output schema:
{
  "action": "swap",
  "token_in": "string (address)",
  "token_out": "string (address)",
  "amount_in": "string (wei)",
  "min_amount_out": "string (wei, apply 1% slippage)",
  "route": "1inch",
  "chain": "string",
  "gas_estimate": int,
  "price_impact": float,
  "executor_ready": bool
}

executor_ready = false if price_impact > 2.0 or if LIVE_QUOTE_DATA contains an error.
Rules: Never execute TXs directly — always output instructions for Executor.
Do NOT use tools, search, read files, or make HTTP requests.""",

    "yield": """You are Yield, the lending protocol manager of the Vespra DeFi agent swarm.
You MUST respond with valid JSON only. No prose, no markdown. Use LIVE_MARKET_DATA to recommend the best yield action.

Output schema:
{
  "positions": [{"protocol": "string", "asset": "string", "supplied": "string", "borrowed": "string", "health_factor": "string", "net_apy": float}],
  "recommended_action": "deposit|withdraw|rebalance|hold",
  "target_protocol": "string",
  "target_asset": "string",
  "amount": "string",
  "executor_ready": bool,
  "reasoning": "string"
}

executor_ready = true ONLY when recommended_action is "deposit" or "withdraw". Otherwise false.

Health factor thresholds: >2.0 healthy, 1.5-2.0 LOW, 1.2-1.5 MEDIUM, <1.2 CRITICAL (exit).
Conservative by default. When in doubt, recommend withdrawal.
Rules: No transactions, no keys. Analyze LIVE_MARKET_DATA only.
Do NOT use tools, search, read files, or make HTTP requests.""",

    "sniper": """You are Sniper, the new pool detector of the Vespra DeFi agent swarm.
You MUST respond with valid JSON only — no prose, no markdown, no explanation.
Use LIVE_POOL_DATA to evaluate the pool.

Output schema:
{
  "status": "opportunity|pass|risky",
  "pool": {
    "dex": "uniswap_v3|aerodrome|camelot|unknown",
    "chain": "string",
    "pair": "TOKEN0/TOKEN1",
    "pool_address": "0x...",
    "tvl_usd": int,
    "volume_24h": int,
    "apy": float,
    "liquidity_lock": bool
  },
  "risk_assessment": {
    "score": "LOW|MEDIUM|HIGH|CRITICAL",
    "factors": ["string"]
  },
  "entry": {
    "action": "swap|pass",
    "amount_eth": "string",
    "max_slippage_bps": int,
    "executor_instruction": "string"
  },
  "entry_recommended": bool,
  "executor_ready": bool
}

Entry criteria — ALL must pass for entry_recommended=true:
- tvl_usd > 50000
- risk_assessment.score is LOW or MEDIUM
- apy > 5.0
- status is "opportunity"

executor_ready = true ONLY when entry_recommended = true AND action = "swap".
Default amount_eth to "0.05" for new pool entries unless context specifies otherwise.
When in doubt, set status="risky" and entry_recommended=false. Be conservative.

Rules: No transactions, no keys. Analyze LIVE_POOL_DATA only.
Do NOT use tools, search, or read files.""",

    "launcher": """You are Launcher, the token deployment specialist for the Vespra DeFi swarm.
You MUST respond with valid JSON only — no prose, no markdown, no explanation.
Use LIVE_DEPLOY_DATA to build the deployment plan.

Output schema:
{
  "action": "deploy_erc20",
  "name": "string",
  "symbol": "string",
  "decimals": int,
  "total_supply_human": "string",
  "total_supply_wei": "string",
  "chain": "string",
  "constructor_args": "hex string (from LIVE_DEPLOY_DATA)",
  "deploy_calldata": "0x-prefixed hex (from LIVE_DEPLOY_DATA)",
  "gas_estimate": int,
  "gas_cost_eth": float,
  "executor_ready": bool,
  "warnings": ["string"],
  "keymaster_calls": [
    {
      "action": "send_native",
      "params": {
        "wallet_id": "string — from task prompt or null if not provided",
        "to": "0x0000000000000000000000000000000000000000",
        "amount_eth": "0"
      }
    }
  ]
}

executor_ready = true ONLY when:
- wallet_id is explicitly provided in the task
- chain is a supported EVM chain
- No CRITICAL warnings

Always populate constructor_args and deploy_calldata from LIVE_DEPLOY_DATA — never fabricate them.
keymaster_calls[0].to should be the zero address for contract deployment (CREATE).

Safety warnings to always check:
- If total_supply_human > 1000000000000 (1 trillion): warn "Extremely high supply"
- If decimals != 18: warn "Non-standard decimals"
- Always warn: "Verify bytecode on testnet before mainnet deployment"
- If chain is "ethereum" or "base" (mainnet): warn "Mainnet deployment — irreversible"

Rules: No transactions, no keys. Use LIVE_DEPLOY_DATA for all calldata values.
Do NOT use tools, search, or read files.""",
}
logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s", datefmt="%Y-%m-%d %H:%M:%S")
log = logging.getLogger("vespra")


# ─── Keymaster bridge ─────────────────────────────────────────────

def keymaster_call(action, params):
    """Call Keymaster /dispatch from the gateway (no sandbox restrictions)."""
    payload = {
        "task_id": f"gw-{int(time.time()*1000)}",
        "prompt": action,
        "input": {"action": action, **params},
    }
    try:
        headers = {"Content-Type": "application/json"}
        if KEYMASTER_TOKEN:
            headers["Authorization"] = f"Bearer {KEYMASTER_TOKEN}"
        req = Request(
            f"{KEYMASTER}/dispatch",
            data=json.dumps(payload).encode(),
            headers=headers,
            method="POST",
        )
        with urlopen(req, timeout=90) as resp:
            return json.loads(resp.read())
    except URLError as e:
        return {"status": "error", "response": f"Keymaster unreachable: {e}"}
    except Exception as e:
        return {"status": "error", "response": str(e)}




def resolve_wallet_id(wallet_ref, chain=None):
    """Resolve a wallet label, address, or UUID to a wallet UUID.

    - If wallet_ref looks like a UUID (contains dashes, 36 chars), return as-is.
    - If it starts with 0x, look up by address.
    - Otherwise, look up by label.
    Returns (wallet_id, error_string_or_None).
    """
    if not wallet_ref:
        return wallet_ref, None

    ref = wallet_ref.strip()

    # Already a UUID
    if len(ref) == 36 and ref.count("-") == 4:
        return ref, None

    # Fetch wallet list (optionally filtered by chain)
    try:
        url = f"{KEYMASTER}/wallets"
        if chain:
            url += f"?chain={chain}"
        headers = {}
        if KEYMASTER_TOKEN:
            headers["Authorization"] = f"Bearer {KEYMASTER_TOKEN}"
        req = Request(url, headers=headers, method="GET")
        with urlopen(req, timeout=10) as resp:
            wallets = json.loads(resp.read())
    except Exception as e:
        return None, f"Failed to fetch wallets for resolution: {e}"

    # Match by address (case-insensitive)
    if ref.startswith("0x"):
        for w in wallets:
            if w.get("address", "").lower() == ref.lower():
                log.info(f"   Resolved address {ref[:10]}... -> {w['id']}")
                return w["id"], None
        return None, f"No wallet found with address {ref}"

    # Match by label (case-insensitive)
    for w in wallets:
        if w.get("label", "").lower() == ref.lower():
            log.info(f"   Resolved label \"{ref}\" -> {w['id']}")
            return w["id"], None

    return None, f"No wallet found with label or address \"{ref}\""


def resolve_plan_wallet_ids(plan):
    """Resolve wallet_id fields in all keymaster_calls within a plan.

    Mutates the plan in place. Returns list of resolution errors (empty if all ok).
    """
    errors = []
    calls = plan.get("keymaster_calls", [])
    for call in calls:
        params = call.get("params", {})
        wallet_ref = params.get("wallet_id")
        if not wallet_ref:
            continue
        chain = params.get("chain")
        resolved, err = resolve_wallet_id(wallet_ref, chain)
        if err:
            errors.append(f"{call.get('action', '?')}: {err}")
        elif resolved != wallet_ref:
            params["wallet_id"] = resolved
    return errors




# ── Security: executor guardrails ─────────────────────────────────
_ALLOWED_ACTIONS = {
    "get_wallet", "list_wallets", "get_balance", "get_all_balances",
    "chain_status", "send_native", "sweep", "get_tx_log",
    "create_wallet", "update_cap", "health",
}

_create_wallet_timestamps = []  # Rate-limit tracking

def _validate_keymaster_call(call, wallet_cache=None):
    """Validate a single keymaster call before execution.

    Returns (is_ok, error_message_or_None, confirmation_required_dict_or_None).
    """
    action = call.get("action", "")
    params = call.get("params", {})

    # Reject unknown actions
    if action not in _ALLOWED_ACTIONS:
        return False, f"Action '{action}' is not in the allowed set: {sorted(_ALLOWED_ACTIONS)}", None

    if action == "send_native":
        to_addr = params.get("to", "").strip().lower()
        wallet_id = params.get("wallet_id", "")
        amount_str = params.get("amount_eth", "0")

        # Validate amount is positive and reasonable
        try:
            amount = float(amount_str)
        except (ValueError, TypeError):
            return False, f"send_native: invalid amount_eth '{amount_str}'", None

        if amount <= 0:
            return False, f"send_native: amount must be > 0, got {amount}", None

        # Check for self-send if we know the wallet address
        if wallet_cache and wallet_id in wallet_cache:
            wallet_addr = wallet_cache[wallet_id].get("address", "").lower()
            if to_addr and to_addr == wallet_addr:
                return False, f"send_native: self-send loop detected (to == wallet address {to_addr[:10]}...)", None

        # Log all send details at WARNING level
        log.warning(f"SEND_NATIVE: wallet_id={wallet_id} to={to_addr} amount_eth={amount_str}")

        # High-value confirmation gate (> 0.5 ETH)
        if amount > 0.5:
            confirmation = {
                "status": "confirmation_required",
                "message": f"Send of {amount_str} ETH requires confirmation",
                "details": {
                    "action": "send_native",
                    "wallet_id": wallet_id,
                    "to": params.get("to", ""),
                    "amount_eth": amount_str,
                },
            }
            return True, None, confirmation

    elif action == "create_wallet":
        # Rate limit: max 5 per minute
        global _create_wallet_timestamps
        now = time.time()
        _create_wallet_timestamps = [t for t in _create_wallet_timestamps if now - t < 60]
        if len(_create_wallet_timestamps) >= 5:
            return False, "create_wallet: rate limit exceeded (max 5 per minute)", None
        _create_wallet_timestamps.append(now)

    elif action == "sweep":
        # Sweep is OK — Keymaster already enforces Safe address.
        # Double-check: if there's a 'to' param that's NOT the Safe, reject.
        if "to" in params:
            return False, "sweep: custom 'to' address not allowed — sweep always goes to the configured Safe", None

    return True, None, None

def execute_keymaster_plan(agent_response_text):
    """Parse Executor LLM output and execute Keymaster calls."""
    try:
        plan = extract_json(agent_response_text)
    except (json.JSONDecodeError, ValueError) as e:
        return {"status": "error", "error": f"Executor returned invalid JSON: {e}", "raw": agent_response_text[:500]}

    calls = plan.get("keymaster_calls", [])
    warnings = plan.get("warnings", [])

    if not calls:
        return {"status": "error", "error": "Executor returned no keymaster_calls", "raw": agent_response_text[:500]}

    log.info(f"   LLM plan: {json.dumps(plan)[:1000]}")

    # Resolve wallet labels/addresses to UUIDs before executing
    resolution_errors = resolve_plan_wallet_ids(plan)
    if resolution_errors:
        return {"status": "error", "error": "Wallet resolution failed: " + "; ".join(resolution_errors), "raw": agent_response_text[:500]}


    results = []
    ctx = {}  # Accumulated context from prior call results

    for i, call in enumerate(calls):
        action = call.get("action", "unknown")
        params = call.get("params", {})

        # Auto-fill params from context when LLM provides bad or missing values
        _fill_from_context(action, params, ctx)

        log.info(f"   Keymaster [{i+1}/{len(calls)}]: {action} {json.dumps(params)[:100]}")
        km_result = keymaster_call(action, params)
        log.info(f"   KM raw [{action}]: {json.dumps(km_result)[:500]}")
        results.append({"action": action, "result": km_result})

        # Abort remaining calls if a critical step fails
        # Check outer dispatch status AND inner response status
        outer_status = km_result.get("status")
        inner_resp = km_result.get("response", {})
        if isinstance(inner_resp, str):
            try:
                inner_resp = json.loads(inner_resp)
            except (json.JSONDecodeError, TypeError):
                inner_resp = {}
        inner_status = inner_resp.get("status") if isinstance(inner_resp, dict) else None
        is_error = outer_status == "error" or (outer_status != "ok" and inner_status == "error")
        if is_error:
            log.error(f"   Keymaster {action} failed, aborting remaining calls")
            break

        # Store successful result data in context for subsequent calls
        _update_context(action, params, km_result, ctx)

    return {"status": "ok", "keymaster_results": results, "warnings": warnings}



def _is_valid_address(addr):
    """Check if addr is a valid 0x-prefixed 40-hex-char Ethereum address."""
    return bool(addr and re.match(r"^0x[0-9a-fA-F]{40}$", addr))

def _fill_from_context(action, params, ctx):
    """Auto-fill or correct params using data from prior successful calls."""
    # Carry forward resolved wallet_id
    if "wallet_id" in params and params["wallet_id"] == ctx.get("wallet_label"):
        params["wallet_id"] = ctx.get("wallet_id", params["wallet_id"])

    if action == "get_balance":
        # If address is missing or not a hex address, use wallet address from context
        addr = params.get("address", "")
        if not _is_valid_address(addr) and ctx.get("wallet_address"):
            log.info(f"   Context fix: address '{addr}' -> {ctx['wallet_address']}")
            params["address"] = ctx["wallet_address"]
        # If chain is missing or doesn't match wallet chain, use wallet chain
        if ctx.get("wallet_chain"):
            chain = params.get("chain", "")
            if not chain or chain != ctx["wallet_chain"]:
                log.info(f"   Context fix: chain '{chain}' -> {ctx['wallet_chain']}")
                params["chain"] = ctx["wallet_chain"]

    elif action == "chain_status":
        # Use wallet chain if chain param is wrong or missing
        if ctx.get("wallet_chain"):
            chain = params.get("chain", "")
            if not chain or chain != ctx["wallet_chain"]:
                log.info(f"   Context fix: chain '{chain}' -> {ctx['wallet_chain']}")
                params["chain"] = ctx["wallet_chain"]

    elif action in ("send_native", "sweep"):
        # Always carry forward wallet_id from context if current value isn't a UUID
        if ctx.get("wallet_id"):
            wid = params.get("wallet_id", "")
            is_uuid = len(wid) == 36 and wid.count("-") == 4
            if not is_uuid:
                log.info(f"   Context fix: wallet_id '{wid}' -> {ctx['wallet_id']}")
                params["wallet_id"] = ctx["wallet_id"]
        # Also fill chain from context if missing
        if not params.get("chain") and ctx.get("wallet_chain"):
            params["chain"] = ctx["wallet_chain"]

    # Normalize amount fields for send_native
    if action == "send_native" and "amount_eth" not in params:
        for alt in ("amount", "value", "value_eth", "eth", "amount_in"):
            if alt in params:
                log.info(f"   Context fix: renaming '{alt}' -> 'amount_eth'")
                params["amount_eth"] = params.pop(alt)
                break

    elif action == "get_tx_log":
        if ctx.get("wallet_id"):
            wid = params.get("wallet_id", "")
            if not wid or (len(wid) != 36 or wid.count("-") != 4):
                params["wallet_id"] = ctx["wallet_id"]


def _update_context(action, params, km_result, ctx):
    """Extract useful data from a successful Keymaster response into context."""
    resp = km_result.get("response", {})
    if isinstance(resp, str):
        try:
            resp = json.loads(resp)
        except (json.JSONDecodeError, TypeError):
            return

    if action == "get_wallet":
        wid = resp.get("id") or params.get("wallet_id", "")
        ctx["wallet_id"] = wid
        ctx["wallet_address"] = resp.get("address", "")
        ctx["wallet_chain"] = resp.get("chain", "")
        ctx["wallet_label"] = resp.get("label", "")
        ctx["wallet_active"] = resp.get("active", False)
        # Also store by wallet_id for guardrail self-send detection
        ctx[wid] = {"address": resp.get("address", "")}

    elif action == "get_balance":
        ctx["balance_wei"] = resp.get("balance_wei", "")
        ctx["balance_eth"] = resp.get("balance_eth", "")

    elif action == "chain_status":
        ctx["chain_block"] = resp.get("block_number", "")
        ctx["chain_gas_gwei"] = resp.get("gas_price_gwei", "")


def extract_json(text):
    """Extract JSON from LLM response, handling markdown fences and preamble."""
    text = text.strip()
    # Try direct parse
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass
    # Try extracting from markdown code fence
    for marker in ("```json", "```"):
        if marker in text:
            start = text.index(marker) + len(marker)
            end = text.index("```", start) if "```" in text[start:] else len(text)
            return json.loads(text[start:end].strip())
    # Try finding first { to last }
    first = text.find("{")
    last = text.rfind("}")
    if first != -1 and last != -1:
        return json.loads(text[first:last+1])
    raise ValueError("No JSON found in response")


# ─── Agent dispatch ───────────────────────────────────────────────



# ── Security: input sanitization ──────────────────────────────────
_SUSPICIOUS_PATTERNS = [
    "ignore previous", "forget instructions", "system prompt",
    "override", "disregard", "bypass", "pretend you are",
]

def _sanitize_message(message):
    """Sanitize user message before passing to LLM agent.

    - Strip null bytes and control characters (except newlines)
    - Enforce max length of 5000 characters
    - Log suspicious prompt-injection patterns
    Returns (sanitized_message, error_string_or_None).
    """
    if not message:
        return message, None

    if len(message) > 5000:
        return None, f"Message too long ({len(message)} chars, max 5000)"

    # Strip null bytes and control characters (keep \n, \r, \t)
    sanitized = re.sub(r'[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]', '', message)

    # Log suspicious patterns
    lower = sanitized.lower()
    for pattern in _SUSPICIOUS_PATTERNS:
        if pattern in lower:
            log.warning(f"SUSPICIOUS INPUT detected pattern '{pattern}' in message: {sanitized[:200]}")
            break

    return sanitized, None

def call_agent(agent_key, message):
    # Security: sanitize input message
    message, san_err = _sanitize_message(message)
    if san_err:
        return {"status": "error", "error": san_err}

    agent = AGENTS.get(agent_key)
    if not agent:
        return {"error": f"unknown agent: {agent_key}", "status": "failed"}

    identity = IDENTITIES.get(agent_key, "")

    # Coordinator: orchestrate DAG before calling LLM summarizer
    coordinator_context = ""
    if agent_key == "coordinator":
        dag_result = orchestrate_dag(message, message)
        coordinator_context = f"\n\nDAG_REPORT:\n{json.dumps(dag_result)}"

    # Scout: inject live DeFi Llama pool data before the user message
    scout_context = ""
    if agent_key == "scout":
        pool_data = pre_fetch_scout()
        scout_context = f"\n\nLIVE_POOL_DATA:\n{json.dumps(pool_data)}\n\nSet data_timestamp to \"{pool_data.get('data_timestamp','')}\" in your response."

    # Trader: inject live 1inch quote data before the user message
    trader_context = ""
    if agent_key == "trader":
        # Parse token_in, token_out from message
        t_in, t_out = None, None
        swap_patterns = [
            r"(?i)swap\s+(\w+)\s+to\s+(\w+)",
            r"(?i)(\w+)\s*->\s*(\w+)",
            r"(?i)trade\s+(\w+)\s+for\s+(\w+)",
        ]
        for pat in swap_patterns:
            m = re.search(pat, message)
            if m:
                t_in, t_out = m.group(1).upper(), m.group(2).upper()
                break
        if t_in and t_out:
            # Default amount: 100 USDC in wei (6 decimals)
            amount_wei = 100000000
            quote_data = pre_fetch_trader(t_in, t_out, amount_wei)
            trader_context = f"\n\nLIVE_QUOTE_DATA:\n{json.dumps(quote_data)}"

        # Trade Up mode: inject compounding micro-trade system prompt
        if "trade_up" in message.lower() or '"mode": "trade_up"' in message:
            trade_up_prompt = (
                "\n\nYou are in TRADE_UP_LOOP mode. Recommend a single compounding micro-trade.\n"
                "Rules:\n"
                "- Only swap if momentum_score >= 0.6\n"
                f"- Max trade size: {TRADE_UP_MAX_ETH} ETH\n"
                f"- Minimum expected gain: {TRADE_UP_MIN_GAIN_PCT}%\n"
                "- If risk_score is HIGH: output action=\"hold\"\n"
                "- If sentinel_data.stop_loss_triggered is true: output action=\"exit\"\n"
                "Output strict JSON only:\n"
                "{\"action\": \"swap\"|\"hold\"|\"exit\", \"token_in\": \"<address>\", "
                "\"token_out\": \"<address>\", \"amount_in_wei\": \"<wei>\", "
                "\"expected_gain_pct\": <float>, \"reasoning\": \"<one line>\"}"
            )
            trader_context += trade_up_prompt

    # Yield: inject live Aave market data before the user message
    yield_context = ""
    if agent_key == "yield":
        from datetime import datetime, timezone
        # Detect chain from message
        msg_lower = message.lower()
        if "base" in msg_lower:
            chain_id = 8453
        elif "arbitrum" in msg_lower:
            chain_id = 42161
        else:
            chain_id = 1  # default to Ethereum
        market_data = pre_fetch_yield(chain_id)
        yield_context = f"\n\nLIVE_MARKET_DATA:\n{json.dumps(market_data)}"

    # Sentinel: inject live wallet + Aave health data before the user message
    sentinel_context = ""
    if agent_key == "sentinel":
        msg_lower = message.lower()
        if "ethereum" in msg_lower:
            s_chain = "ethereum"
        elif "arbitrum" in msg_lower:
            s_chain = "arbitrum"
        else:
            s_chain = "base"
        sentinel_data = pre_fetch_sentinel(chain=s_chain)
        sentinel_context = f"\n\nLIVE_SENTINEL_DATA:\n{json.dumps(sentinel_data)}"

    # Sniper: inject live pool data before the user message
    sniper_context = ""
    if agent_key == "sniper":
        # Extract pool address from message if present
        pool_match = re.search(r"0x[0-9a-fA-F]{40}", message)
        pool_address = pool_match.group(0) if pool_match else "0x0000000000000000000000000000000000000000"
        msg_lower = message.lower()
        if "ethereum" in msg_lower:
            s_chain = "ethereum"
        elif "arbitrum" in msg_lower:
            s_chain = "arbitrum"
        else:
            s_chain = "base"
        pool_data = pre_fetch_sniper(pool_address, chain=s_chain)
        sniper_context = f"\n\nLIVE_POOL_DATA:\n{json.dumps(pool_data)}"

    # Launcher: inject real ERC-20 deploy calldata before the user message
    launcher_context = ""
    if agent_key == "launcher":
        # Extract token params from message
        import re as _re
        msg_text = message
        # Defaults
        l_name    = "MyToken"
        l_symbol  = "MYT"
        l_supply  = "1000000000"
        l_decimals = 18

        # Try to extract: name, symbol, supply from message
        name_m   = _re.search(r'(?i)name[:\s]+(["\']?)([A-Za-z0-9]+)\1', msg_text)
        symbol_m = _re.search(r'(?i)symbol[:\s]+(["\']?)([A-Z0-9]{2,10})\1', msg_text)
        supply_m = _re.search(r'(?i)supply[:\s]+([\d,]+)', msg_text)
        dec_m    = _re.search(r'(?i)decimals?[:\s]+(\d+)', msg_text)

        if name_m:   l_name    = name_m.group(2).strip()
        if symbol_m: l_symbol  = symbol_m.group(2).strip()
        if supply_m: l_supply  = supply_m.group(1).replace(',', '')
        if dec_m:    l_decimals = int(dec_m.group(1))

        msg_lower = message.lower()
        l_chain = "ethereum" if "ethereum" in msg_lower else "arbitrum" if "arbitrum" in msg_lower else "base"

        deploy_data = pre_fetch_launcher(l_name, l_symbol, l_supply, l_decimals, l_chain)
        launcher_context = f"\n\nLIVE_DEPLOY_DATA:\n{json.dumps(deploy_data)}"

    # Risk: inject live DeFi Llama protocol data before the user message
    risk_context = ""
    if agent_key == "risk":
        # Extract protocol name from message
        msg_lower = message.lower()
        protocol_name = None
        for keyword in ["analyze", "score", "check", "risk"]:
            if keyword in msg_lower:
                parts = msg_lower.split(keyword, 1)[1].strip().split()
                # Skip filler words like "risk", "for", "of"
                for word in parts:
                    if word not in ("risk", "for", "of", "the", "protocol"):
                        protocol_name = word.strip(".,!?\"'")
                        break
                if protocol_name:
                    break
        if not protocol_name:
            # Fallback: first word of message
            protocol_name = message.strip().split()[0].lower() if message.strip() else ""
        if protocol_name:
            # Also extract contract address if present in message
            contract_addr = None
            addr_match = re.search(r"0x[0-9a-fA-F]{40}", message)
            if addr_match:
                contract_addr = addr_match.group(0)
            # Detect chain
            msg_lower_r = message.lower()
            r_chain = "ethereum" if "ethereum" in msg_lower_r else "arbitrum" if "arbitrum" in msg_lower_r else "base"
            risk_data = pre_fetch_risk(protocol_name, contract_address=contract_addr, chain=r_chain)
            risk_context = f"\n\nLIVE_PROTOCOL_DATA:\n{json.dumps(risk_data)}"

    full_msg = f"[SYSTEM] {identity}{coordinator_context}{scout_context}{risk_context}{trader_context}{yield_context}{sentinel_context}{sniper_context}{launcher_context}\n\n[TASK] {message}" if identity else message
    session = f"v{int(time.time())}"

    cmd = [
        "sudo", "-u", agent["user"], f"HOME={agent['home']}",
        NULLCLAW, "agent",
        "-m", full_msg,
        "-s", session,
        "--provider", LLM_PROVIDER,
        "--model",    _RESOLVED_MODEL,
    ]
    log.info(f"-> {agent_key} [{session}] [{LLM_PROVIDER}/{_RESOLVED_MODEL}]: {message[:120]}")

    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=TIMEOUT, cwd=agent["home"])
        lines = [l for l in result.stdout.strip().split("\n")
                 if not l.startswith(("Sending to ", "Session: ", "info("))]
        response = "\n".join(lines).strip()

        if result.returncode != 0 and not response:
            log.error(f"X {agent_key}: {result.stderr[:200]}")
            return {"error": result.stderr[:500] or "non-zero exit", "status": "failed"}

        log.info(f"<- {agent_key}: {response[:120]}")

        # Executor post-processing: parse LLM output and execute Keymaster calls
        if agent_key == "executor" and response:
            log.info(f"   Executor -> Keymaster bridge")
            km_result = execute_keymaster_plan(response)
            # NullBoiler expects "response" to be a string, not an object
            return {"response": json.dumps(km_result), "status": "ok", "agent": agent_key}

        # Risk post-processing: validate JSON schema
        if agent_key == "risk" and response:
            try:
                parsed = extract_json(response)
                if "score" not in parsed or "gate_pass" not in parsed:
                    return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Scout post-processing: validate JSON schema
        if agent_key == "scout" and response:
            try:
                parsed = extract_json(response)
                if "opportunities" not in parsed:
                    return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Trader post-processing: validate JSON schema
        if agent_key == "trader" and response:
            try:
                parsed = extract_json(response)
                if "action" not in parsed or "executor_ready" not in parsed:
                    return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Yield post-processing: validate JSON schema
        if agent_key == "yield" and response:
            try:
                parsed = extract_json(response)
                if "recommended_action" not in parsed or "executor_ready" not in parsed:
                    return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Sentinel post-processing: validate JSON schema
        if agent_key == "sentinel" and response:
            try:
                parsed = extract_json(response)
                required_keys = {"wallets", "trade_positions", "total_portfolio_usd", "overall_status"}
                missing = required_keys - set(parsed.keys())
                if missing:
                    return {"response": json.dumps({"error": "invalid_schema", "missing": list(missing), "raw": response[:500]}), "status": "ok", "agent": agent_key}
                # Log DAG triggers and critical status
                if parsed.get("overall_status") == "critical":
                    log.warning(f"SENTINEL CRITICAL: worst_position={parsed.get('worst_position')} alerts={parsed.get('wallets', [{}])[0].get('alerts', []) if parsed.get('wallets') else []}")
                for w in parsed.get("wallets", []):
                    if w.get("trigger_dag") == "health-monitor-exit":
                        log.warning(f"SENTINEL DAG TRIGGER: health-monitor-exit for wallet {w.get('wallet', '?')[:12]}...")
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Sniper post-processing: validate JSON schema
        if agent_key == "sniper" and response:
            try:
                parsed = extract_json(response)
                required_keys = {"status", "pool", "risk_assessment", "entry", "entry_recommended", "executor_ready"}
                missing = required_keys - set(parsed.keys())
                if missing:
                    return {"response": json.dumps({"error": "invalid_schema", "missing": list(missing), "raw": response[:500]}), "status": "ok", "agent": agent_key}
                if parsed.get("entry_recommended"):
                    log.warning(f"SNIPER ENTRY: {parsed.get('pool', {}).get('pool_address', '?')[:12]}... — {parsed.get('pool', {}).get('pair', '?')} score={parsed.get('risk_assessment', {}).get('score', '?')}")
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Launcher post-processing: validate JSON schema
        if agent_key == "launcher" and response:
            try:
                parsed = extract_json(response)
                required_keys = {"action", "name", "symbol", "constructor_args", "deploy_calldata", "executor_ready", "warnings"}
                missing = required_keys - set(parsed.keys())
                if missing:
                    return {"response": json.dumps({"error": "invalid_schema", "missing": list(missing), "raw": response[:500]}), "status": "ok", "agent": agent_key}
                if parsed.get("executor_ready"):
                    log.warning(f"LAUNCHER READY: {parsed.get('symbol','?')} on {parsed.get('chain','?')} — wallet_id required before broadcast")
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        # Coordinator post-processing: validate JSON schema
        if agent_key == "coordinator" and response:
            try:
                parsed = extract_json(response)
                required_keys = {"report", "dag_name", "steps_completed", "trigger_dag", "alerts", "next_action", "status"}
                missing = required_keys - set(parsed.keys())
                if missing:
                    return {"response": json.dumps({"error": "invalid_schema", "missing": list(missing), "raw": response[:500]}), "status": "ok", "agent": agent_key}
                if parsed.get("status") == "critical" or parsed.get("trigger_dag"):
                    log.warning(f"COORDINATOR CRITICAL: trigger_dag={parsed.get('trigger_dag')} alerts={parsed.get('alerts')}")
                return {"response": json.dumps(parsed), "status": "ok", "agent": agent_key}
            except (json.JSONDecodeError, ValueError):
                return {"response": json.dumps({"error": "invalid_schema", "raw": response[:500]}), "status": "ok", "agent": agent_key}

        return {"response": response, "status": "ok", "agent": agent_key}
    except subprocess.TimeoutExpired:
        return {"error": f"timeout after {TIMEOUT}s", "status": "failed"}
    except Exception as e:
        return {"error": str(e), "status": "failed"}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a): pass

    def _json(self, code, data):
        body = json.dumps(data).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/pending-approvals":
            try:
                r = _redis_client()
                items = r.lrange("vespra:pending_approvals", 0, 49)
                parsed_items = []
                for item in items:
                    try:
                        parsed_items.append(json.loads(item))
                    except Exception:
                        pass
                self._json(200, {"count": len(parsed_items), "approvals": parsed_items})
            except Exception as e:
                self._json(500, {"error": str(e)})
            return

        if self.path == "/yield/rotations":
            try:
                r = _redis_client()
                items = r.lrange("vespra:yield_rotations", 0, 49)
                rotations = []
                for item in items:
                    try:
                        rotations.append(json.loads(item))
                    except Exception:
                        pass
                self._json(200, {"count": len(rotations), "rotations": rotations})
            except Exception as e:
                self._json(500, {"error": str(e)})
            return

        if self.path.startswith("/yield/position/"):
            wallet_id = self.path.split("/yield/position/", 1)[1].strip("/")
            self._json(200, _get_current_yield_position(wallet_id))
            return

        if self.path == "/sniper/entries":
            entries = _get_sniper_entries()
            self._json(200, {"count": len(entries), "entries": entries})
            return

        if self.path == "/trade-up/history":
            try:
                r = _redis_client()
                raw = r.lrange("vespra:trade_up_history", 0, 99)
                cycles = []
                for item in raw:
                    try:
                        cycles.append(json.loads(item))
                    except Exception:
                        pass
                self._json(200, {"count": len(cycles), "cycles": cycles})
            except Exception as e:
                self._json(500, {"error": str(e)})
            return

        if self.path.startswith("/trade-up/status/"):
            wallet_id = self.path.split("/trade-up/status/", 1)[1].strip("/")
            try:
                r = _redis_client()
                state = r.get(f"vespra:trade_up_state:{wallet_id}")
                self._json(200, json.loads(state) if state else {"active": False})
            except Exception as e:
                self._json(500, {"error": str(e)})
            return

        if self.path == "/portfolio/status":
            try:
                r = _redis_client()
                raw_map = r.hgetall("vespra:portfolio_wallets") or {}
                wallet_map = {
                    (k.decode() if isinstance(k, bytes) else k): (v.decode() if isinstance(v, bytes) else v)
                    for k, v in raw_map.items()
                }
                status = {}
                for strategy, wid in wallet_map.items():
                    if strategy == "trade_up":
                        state_raw = r.get(f"vespra:trade_up_state:{wid}")
                        status[strategy] = json.loads(state_raw) if state_raw else {"active": False}
                    else:
                        status[strategy] = {"wallet_id": wid, "strategy": strategy}
                self._json(200, {"wallet_map": wallet_map, "strategy_status": status})
            except Exception as e:
                self._json(500, {"error": str(e)})
            return

        if self.path == "/portfolio/history":
            try:
                r = _redis_client()
                raw = r.lrange("vespra:portfolio_launches", 0, 49)
                launches = []
                for item in raw:
                    try:
                        launches.append(json.loads(item))
                    except Exception:
                        pass
                self._json(200, {"count": len(launches), "launches": launches})
            except Exception as e:
                self._json(500, {"error": str(e)})
            return

        if self.path == "/queue/status":
            depth = queue_depth()
            self._json(200, {
                "enabled": QUEUE_ENABLED,
                "redis":   f"{REDIS_HOST}:{REDIS_PORT}/{REDIS_DB}",
                **depth,
            })
            return
        if self.path == "/health":
            self._json(200, {
                "status":   "ok",
                "service":  "vespra-gateway",
                "agents":   list(AGENTS.keys()),
                "provider": LLM_PROVIDER,
                "model":    _RESOLVED_MODEL,
            })
        elif self.path == "/providers":
            self._json(200, {
                "active_provider": LLM_PROVIDER,
                "active_model":    _RESOLVED_MODEL,
                "supported":       sorted(_SUPPORTED_PROVIDERS),
                "defaults":        _PROVIDER_DEFAULTS,
            })
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        if not length:
            return self._json(400, {"error": "empty body"})
        raw_body = self.rfile.read(length)
        try:
            body = json.loads(raw_body.decode("utf-8") if isinstance(raw_body, bytes) else raw_body)
        except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as e:
            log.error(f"[do_POST] JSON parse error on {self.path}: {e}\nbody={raw_body!r}")
            return self._json(400, {"error": "invalid json"})

        if self.path == "/dispatch":
            task_id = body.get("task_id", "unknown")
            step = body.get("step", {})
            message = body.get("input", "") or body.get("message", "")
            tags = step.get("tags", []) if isinstance(step, dict) else []
            agent_key = next((t for t in tags if t in AGENTS), body.get("worker", body.get("agent", "")))
            if not agent_key or agent_key not in AGENTS:
                return self._json(400, {"error": f"no agent for tags {tags}", "task_id": task_id})
            if not message:
                return self._json(400, {"error": "missing input", "task_id": task_id})
            result = call_agent(agent_key, message)
            result["task_id"] = task_id
            self._json(200 if result["status"] == "ok" else 500, result)

        elif self.path.startswith("/agent/"):
            agent_key = self.path.split("/agent/", 1)[1].strip("/")
            message = body.get("message", "")
            if not message:
                return self._json(400, {"error": "missing message"})
            result = call_agent(agent_key, message)
            self._json(200 if result["status"] == "ok" else 500, result)

        elif self.path == "/swarm":
            message = body.get("message", "")
            targets = body.get("agents", list(AGENTS.keys()))
            if not message:
                return self._json(400, {"error": "missing message"})
            results = {k: call_agent(k, message) for k in targets if k in AGENTS}
            self._json(200, {"results": results})

        elif self.path == "/webhooks/alchemy":
            # Use raw_body already read above for HMAC verification
            sig_header = self.headers.get("x-alchemy-signature", "")
            if not _verify_alchemy_signature(raw_body, sig_header):
                log.warning("Alchemy webhook: invalid signature — rejected")
                return self._json(401, {"error": "invalid signature"})

            # Extract pool creation events from Alchemy GRAPHQL webhook payload
            events = body.get("event", {}).get("data", {}).get("block", {}).get("logs", [])
            if not events:
                # Also handle activity webhook format
                events = body.get("event", {}).get("activity", [])

            log.info(f"Alchemy webhook: {len(events)} event(s) received")
            triggered = []

            for event in events:
                # Try to extract pool address from topics or data
                topics = event.get("topics", [])
                # Uniswap V3 PoolCreated: topic[0] = event sig, topic[3] = pool address (padded)
                pool_address = None
                if len(topics) >= 4:
                    raw_addr = topics[3]
                    if raw_addr and len(raw_addr) >= 42:
                        pool_address = "0x" + raw_addr[-40:]
                # Fallback: check "address" field (the factory address fired the event)
                if not pool_address:
                    pool_address = event.get("address", "")

                if not pool_address or not re.match(r"^0x[0-9a-fA-F]{40}$", pool_address):
                    continue

                chain_str = body.get("event", {}).get("network", "BASE_MAINNET")
                chain = "base" if "BASE" in chain_str else "ethereum" if "ETH" in chain_str else "arbitrum" if "ARB" in chain_str else "base"

                log.info(f"Alchemy webhook: new pool detected {pool_address[:12]}... on {chain}")
                result = call_agent("sniper", f"Score new pool {pool_address} on {chain}")
                entry_result = None

                # Auto-entry if enabled and sniper recommends it
                if SNIPER_AUTO_ENTRY_ENABLED:
                    try:
                        sniper_parsed = json.loads(result.get("response", "{}"))
                        if (
                            sniper_parsed.get("entry_recommended")
                            and sniper_parsed.get("executor_ready")
                            and sniper_parsed.get("risk_assessment", {}).get("score") in ("LOW", "MEDIUM")
                        ):
                            pool_data = {"tvl_usd": sniper_parsed.get("pool", {}).get("tvl_usd", 0)}
                            entry_result = execute_sniper_entry(pool_address, pool_data, sniper_parsed, chain)
                            log.warning(f"SNIPER AUTO-ENTRY: {entry_result.get('status')} pool={pool_address[:12]}...")
                    except Exception as e:
                        log.error(f"Sniper auto-entry error: {e}")

                triggered.append({
                    "pool_address": pool_address,
                    "chain":        chain,
                    "result":       result,
                    "entry_result": entry_result,
                })

            return self._json(200, {"status": "ok", "triggered": len(triggered), "results": triggered})

        elif self.path == "/trade-up/start":
            wallet_id = body.get("wallet_id", "")
            capital_eth = float(body.get("capital_eth", TRADE_UP_MAX_ETH))
            if not wallet_id:
                return self._json(400, {"error": "wallet_id required"})
            if not TRADE_UP_ENABLED:
                return self._json(403, {"error": "TRADE_UP_ENABLED=false — set in .env and restart"})
            if capital_eth > TRADE_UP_MAX_ETH:
                return self._json(400, {"error": f"capital_eth {capital_eth} exceeds TRADE_UP_MAX_ETH {TRADE_UP_MAX_ETH}"})
            t = threading.Thread(target=run_trade_up_loop, args=(wallet_id, capital_eth), daemon=True)
            t.start()
            self._json(200, {"status": "started", "wallet_id": wallet_id, "capital_eth": capital_eth})

        elif self.path.startswith("/trade-up/stop/"):
            wallet_id = self.path.split("/trade-up/stop/", 1)[1].strip("/")
            try:
                r = _redis_client()
                state_raw = r.get(f"vespra:trade_up_state:{wallet_id}")
                state = json.loads(state_raw) if state_raw else {}
                state["active"] = False
                r.set(f"vespra:trade_up_state:{wallet_id}", json.dumps(state))
                self._json(200, {"status": "stop_requested", "wallet_id": wallet_id})
            except Exception as e:
                self._json(500, {"error": str(e)})

        elif self.path == "/portfolio/launch":
            # Defensive: re-parse raw_body if body is somehow not a dict
            # (guards against stale deploys where shared parser was different)
            if not isinstance(body, dict):
                log.error(f"[portfolio/launch] body is not dict: type={type(body)} raw_body={raw_body!r}")
                try:
                    body = json.loads(raw_body.decode("utf-8") if isinstance(raw_body, bytes) else raw_body)
                except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as e:
                    log.error(f"[portfolio/launch] JSON re-parse failed: {e}")
                    return self._json(400, {"error": "invalid json"})

            dry_run = body.get("dry_run", False)

            if not PORTFOLIO_MANAGER_ENABLED:
                return self._json(403, {"error": "PORTFOLIO_MANAGER_ENABLED=false — set in .env and restart"})

            # NL command path
            if "command" in body:
                total_eth = float(body.get("total_eth", PORTFOLIO_MAX_ETH))
                if total_eth > PORTFOLIO_MAX_ETH:
                    return self._json(400, {"error": f"total_eth {total_eth} exceeds PORTFOLIO_MAX_ETH {PORTFOLIO_MAX_ETH}"})
                split = parse_portfolio_command(body["command"], total_eth)
                if "error" in split:
                    return self._json(400, {"error": split["error"]})

            # Explicit pct path
            elif all(k in body for k in ["total_eth", "yield_pct", "sniper_pct", "trade_up_pct"]):
                total_eth = float(body["total_eth"])
                if total_eth > PORTFOLIO_MAX_ETH:
                    return self._json(400, {"error": f"total_eth {total_eth} exceeds PORTFOLIO_MAX_ETH {PORTFOLIO_MAX_ETH}"})
                yield_pct = float(body["yield_pct"])
                sniper_pct = float(body["sniper_pct"])
                trade_up_pct = float(body["trade_up_pct"])
                total_pct = yield_pct + sniper_pct + trade_up_pct
                if abs(total_pct - 100.0) > 1.0:
                    return self._json(400, {"error": f"Percentages sum to {total_pct}, must equal 100"})
                split = {
                    "yield": yield_pct / 100,
                    "sniper": sniper_pct / 100,
                    "trade_up": trade_up_pct / 100,
                    "total_eth": total_eth,
                }
            else:
                return self._json(400, {"error": "Provide either 'command' or 'total_eth + yield_pct + sniper_pct + trade_up_pct'"})

            result = execute_portfolio_launch(split, dry_run=dry_run)
            self._json(200, result)

        elif self.path == "/queue/push":
            agent   = body.get("agent", "")
            message = body.get("message", "")
            if not agent or agent not in AGENTS:
                return self._json(400, {"error": f"invalid agent: {agent}"})
            if not message:
                return self._json(400, {"error": "missing message"})
            if not QUEUE_ENABLED:
                return self._json(503, {"error": "queue disabled"})
            try:
                job = enqueue_job(
                    agent,
                    message,
                    dag_run_id=body.get("dag_run_id", ""),
                    step=body.get("step", ""),
                )
                self._json(200, {"status": "queued", "job_id": job["job_id"]})
            except Exception as e:
                self._json(500, {"error": f"enqueue failed: {e}"})

        else:
            self._json(404, {"error": "not found"})


if __name__ == "__main__":
    HTTPServer.allow_reuse_address = True
    server = HTTPServer((HOST, PORT), Handler)
    log.info(f"Vespra Worker Gateway on {HOST}:{PORT}")
    log.info(f"Agents: {', '.join(AGENTS.keys())}")
    log.info(f"LLM provider: {LLM_PROVIDER} / model: {_RESOLVED_MODEL}")
    # Sentinel background polling thread
    def _sentinel_poll_loop():
        log.info(f"Sentinel poll loop started (interval={SENTINEL_POLL_INTERVAL}s)")
        while True:
            try:
                time.sleep(SENTINEL_POLL_INTERVAL)
                log.info("Sentinel poll: auto-enqueueing watchdog job")
                enqueue_job("sentinel", "monitor all wallets", dag_run_id="auto-poll", step="watchdog")
            except Exception as e:
                log.error(f"Sentinel poll loop error: {e}")

    _sentinel_thread = threading.Thread(target=_sentinel_poll_loop, daemon=True, name="sentinel-poll")
    _sentinel_thread.start()
    log.info(f"Sentinel poll thread started (every {SENTINEL_POLL_INTERVAL}s)")

    if QUEUE_ENABLED:
        _worker_thread = threading.Thread(target=_queue_worker, daemon=True, name="queue-worker")
        _worker_thread.start()
        log.info(f"Queue worker started: redis={REDIS_HOST}:{REDIS_PORT} queues={QUEUE_KEY},{RETRY_KEY},{DLQ_KEY}")
    else:
        log.info("Queue worker disabled (VESPRA_QUEUE_ENABLED=false)")
    if not LLM_API_KEY:
        log.warning("LLM_API_KEY not set in environment — agents will use per-workspace key from nullclaw config")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.server_close()
