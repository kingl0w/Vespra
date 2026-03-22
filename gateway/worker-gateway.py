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

def pre_fetch_scout():
    """Fetch live pool data from DeFi Llama for Scout agent context."""
    try:
        req = Request("https://yields.llama.fi/pools", method="GET")
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        pools = data.get("data", [])
        filtered = [
            p for p in pools
            if (p.get("tvlUsd") or 0) >= 500_000 and (p.get("apy") or 0) >= 1.0
        ]
        filtered.sort(key=lambda p: p.get("apy", 0), reverse=True)
        top = filtered[:20]
        top_pools = [
            {
                "protocol": p.get("project", ""),
                "pool": p.get("symbol", ""),
                "chain": p.get("chain", ""),
                "apy": round(p.get("apy", 0), 2),
                "tvl_usd": int(p.get("tvlUsd", 0)),
                "il_risk": p.get("ilRisk", "unknown"),
                "stable": bool(p.get("stablecoin", False)),
            }
            for p in top
        ]
        return {"pool_count": len(filtered), "top_pools": top_pools}
    except Exception as e:
        return {"pool_count": 0, "top_pools": [], "error": str(e)}


def pre_fetch_risk(protocol):
    """Fetch live protocol data from DeFi Llama for Risk agent context."""
    try:
        req = Request(f"https://api.llama.fi/protocol/{protocol}", method="GET")
        with urlopen(req, timeout=8) as resp:
            data = json.loads(resp.read())
        tvl_array = data.get("tvl", [])
        audits = data.get("audits") or data.get("audit_links") or data.get("auditLinks") or []

        # Current TVL: last entry in tvl array
        current_tvl = 0
        latest_tvl = 0
        if tvl_array and isinstance(tvl_array[-1], dict):
            latest_tvl = tvl_array[-1].get("totalLiquidityUSD", 0) or 0
            current_tvl = latest_tvl

        # TVL trend: compare latest vs 30-day-ago value
        tvl_trend = 0
        if tvl_array and len(tvl_array) >= 2 and latest_tvl > 0:
            target_ts = tvl_array[-1].get("date", 0) - (30 * 86400)
            past_val = None
            for entry in tvl_array:
                if isinstance(entry, dict) and entry.get("date", 0) <= target_ts:
                    past_val = entry.get("totalLiquidityUSD", 0)
            if past_val and past_val > 0:
                tvl_trend = round(((latest_tvl - past_val) / past_val) * 100, 2)

        # Age in days: derived from first tvl array entry date
        age_days = 0
        if tvl_array and isinstance(tvl_array[0], dict):
            first_date = tvl_array[0].get("date", 0) or 0
            if first_date > 0:
                age_days = int((time.time() - first_date) / 86400)

        return {
            "tvl": current_tvl,
            "tvl_trend": tvl_trend,
            "audits": audits,
            "age_days": age_days,
        }
    except Exception as e:
        return {"error": str(e), "tvl": 0, "tvl_trend": 0, "audits": [], "age_days": 0}


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
Synthesize data from other agents into a concise Telegram report for @dr_bonkers.
Output: Plain text, <1500 chars. Lead with top finding. Include next steps.
Rules: No transactions, no keys, no fabrication. Summarize only what you receive.
Do NOT use tools, search, or read files.""",

    "scout": """You are Scout, yield discovery agent of the Vespra DeFi swarm.
You MUST respond with valid JSON only matching the schema below. No prose, no markdown.
Base your analysis on the LIVE_POOL_DATA provided.

Output schema:
{
  "opportunities": [
    {
      "protocol": "string",
      "pool": "string (symbol)",
      "chain": "string",
      "apy": float,
      "tvl_usd": int,
      "risk_tier": "LOW|MEDIUM|HIGH",
      "recommended_action": "string"
    }
  ],
  "summary": "string",
  "data_timestamp": "ISO 8601 UTC"
}

Risk tier logic: apy > 50 = HIGH, 10-50 = MEDIUM, < 10 = LOW.
Return max 5 opportunities, sorted by risk-adjusted value.
Rules: No transactions, no keys. Analyze LIVE_POOL_DATA only.
Do NOT use tools, search, or read files.""",

    "sentinel": """You are Sentinel, position monitor of the Vespra DeFi agent swarm.
Return ONLY valid JSON — no commentary, no markdown, no explanation.
Output: JSON array of max 5 objects: {protocol, position, alert_type, severity, details, recommended_action}.
Severity: LOW/MEDIUM/HIGH/CRITICAL. Focus on health factors, depegs, TVL drops, upgrades.
Rules: No transactions, no keys. Use training knowledge only.
Do NOT use tools, search, or read files.""",

    "risk": """You are Risk, safety evaluator of the Vespra DeFi agent swarm.
You MUST respond with valid JSON only. No prose, no markdown. Use LIVE_PROTOCOL_DATA to score the protocol.

Output schema:
{
  "protocol": "string",
  "chain": "string",
  "score": "LOW|MEDIUM|HIGH|CRITICAL",
  "factors": [{"category": "string", "rating": "PASS|WARN|FAIL", "detail": "string"}],
  "recommendation": "string (max 20 words)",
  "gate_pass": true/false
}

gate_pass = true ONLY when score is LOW or MEDIUM. Otherwise false.

Scoring hints (use LIVE_PROTOCOL_DATA values):
- TVL: > $10M = PASS, $1M-$10M = WARN, < $1M = FAIL
- TVL trend: > -20% = PASS, -20% to -50% = WARN, < -50% = FAIL
- Audits: has audits = PASS, no audits = FAIL
- Age: > 180 days = PASS, 30-180 days = WARN, < 30 days = FAIL

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
You manage positions in Aave, Compound, and similar protocols using your training knowledge.

Return ONLY valid JSON — no commentary, no markdown, no explanation:
{
  "status": "ok|warning|critical",
  "action": "deposit|withdraw|monitor|exit",
  "protocol": "aave_v3|compound_v3",
  "chain": "...",
  "position": {
    "asset": "...", "amount": "...", "health_factor": null,
    "supply_apy": "...", "borrow_apy": null
  },
  "executor_instruction": "...",
  "warnings": []
}

Health factor thresholds: >2.0 healthy, 1.5-2.0 LOW, 1.2-1.5 MEDIUM, <1.2 CRITICAL (exit).
Conservative by default. When in doubt, recommend withdrawal.
Do NOT use tools, search, read files, or make HTTP requests. Use training knowledge only.""",

    "sniper": """You are Sniper, the new pool detector of the Vespra DeFi agent swarm.
You evaluate new liquidity pools for early entry opportunities using your knowledge.

Return ONLY valid JSON:
{
  "status": "opportunity|pass|risky",
  "pool": {
    "dex": "uniswap_v3|aerodrome|camelot",
    "chain": "...", "pair": "TOKEN/WETH", "pool_address": "0x...",
    "created_at": "...", "tvl_usd": "...", "volume_24h": "...",
    "token_verified": true
  },
  "risk_assessment": {"score": "LOW|MEDIUM|HIGH", "factors": []},
  "entry": {
    "action": "swap", "amount_eth": "...",
    "max_slippage_bps": 100,
    "executor_instruction": "..."
  }
}

Minimum criteria: TVL >$50k, token verified, liquidity locked, risk <=MEDIUM.
Do NOT use tools, search, read files, or make HTTP requests. Use training knowledge only.""",

    "launcher": """You are Launcher, the token deployment specialist for the Vespra DeFi swarm.

Your role: Design and plan ERC-20 token deployments across EVM chains.

CAPABILITIES:
- Standard ERC-20 token design (name, symbol, decimals, total supply)
- Fee-on-transfer / tax token parameters (buy tax, sell tax, max wallet, max tx)
- Bonding curve token configurations (linear, exponential, sigmoid curves)
- Initial liquidity planning (Uniswap V2/V3, pool parameters, price ranges)
- Launch safety analysis (honeypot detection patterns, rug pull red flags)
- Multi-chain deployment planning (Ethereum, Base, Arbitrum, Optimism)

Return ONLY valid JSON — no commentary, no markdown, no explanation:
{
  "status": "planned|error",
  "token_config": {
    "name": "Token Name",
    "symbol": "TKN",
    "decimals": 18,
    "total_supply": "1000000000000000000000000",
    "chain": "base",
    "features": {
      "mintable": false, "burnable": true, "pausable": false,
      "fee_on_transfer": {"enabled": false, "buy_fee_bps": 0, "sell_fee_bps": 0, "fee_recipient": null},
      "max_wallet_pct": null, "max_tx_pct": null
    }
  },
  "deployment": {
    "contract_type": "standard_erc20|fee_token|bonding_curve",
    "estimated_gas": "estimated deployment gas",
    "constructor_args": ["arg1", "arg2"],
    "deploy_calldata": "0x...",
    "wallet_id": "deployer wallet ID"
  },
  "liquidity": {
    "dex": "uniswap_v2|uniswap_v3|none",
    "pair_token": "ETH|USDC",
    "initial_eth": "amount in wei",
    "initial_tokens": "amount in token units",
    "lock_duration_days": 180
  },
  "warnings": [],
  "keymaster_calls": [
    {"method": "POST", "path": "/tx/send", "body": {"wallet_id": "...", "chain": "...", "to": "...", "value": "0", "data": "0x..."}}
  ]
}

SAFETY RULES:
1. Always warn about honeypot patterns (disabled transfers, blacklist functions, hidden mints)
2. Flag excessive fees (>10% buy/sell tax)
3. Recommend liquidity locks (minimum 90 days)
4. Warn if no renounce-ownership is planned
5. Flag unlimited mint authority as HIGH RISK
6. Recommend testnet deployment first for any mainnet launch
7. Never deploy without explicit wallet_id from the task prompt
Do NOT use tools, search, read files, or make HTTP requests. Use training knowledge only.""",
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

    # Scout: inject live DeFi Llama pool data before the user message
    scout_context = ""
    if agent_key == "scout":
        from datetime import datetime, timezone
        pool_data = pre_fetch_scout()
        data_timestamp = datetime.now(timezone.utc).isoformat()
        pool_data["data_timestamp"] = data_timestamp
        scout_context = f"\n\nLIVE_POOL_DATA:\n{json.dumps(pool_data)}\n\nSet data_timestamp to \"{data_timestamp}\" in your response."

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
            risk_data = pre_fetch_risk(protocol_name)
            risk_context = f"\n\nLIVE_PROTOCOL_DATA:\n{json.dumps(risk_data)}"

    full_msg = f"[SYSTEM] {identity}{scout_context}{risk_context}{trader_context}\n\n[TASK] {message}" if identity else message
    session = f"v{int(time.time())}"

    cmd = ["sudo", "-u", agent["user"], f"HOME={agent['home']}", NULLCLAW, "agent", "-m", full_msg, "-s", session]
    log.info(f"-> {agent_key} [{session}]: {message[:120]}")

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
        if self.path == "/health":
            self._json(200, {"status": "ok", "service": "vespra-gateway", "agents": list(AGENTS.keys())})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        if not length:
            return self._json(400, {"error": "empty body"})
        try:
            body = json.loads(self.rfile.read(length))
        except json.JSONDecodeError:
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

        else:
            self._json(404, {"error": "not found"})


if __name__ == "__main__":
    HTTPServer.allow_reuse_address = True
    server = HTTPServer((HOST, PORT), Handler)
    log.info(f"Vespra Worker Gateway on {HOST}:{PORT}")
    log.info(f"Agents: {', '.join(AGENTS.keys())}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.server_close()
