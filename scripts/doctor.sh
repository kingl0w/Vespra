#!/usr/bin/env bash
# Vespra health check. Non-interactive — exit 0 if all checks pass, 1 otherwise.
# Use after `make up` to verify every service is responsive.
set -u

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ENV_FILE=".env"

GREEN="\033[32m"
RED="\033[31m"
DIM="\033[2m"
BOLD="\033[1m"
RESET="\033[0m"

PASS=0
FAIL=0

pass() { printf "${GREEN}✅${RESET} %s\n" "$1"; PASS=$((PASS + 1)); }
fail() { printf "${RED}❌${RESET} %s\n" "$1"; FAIL=$((FAIL + 1)); }
info() { printf "${DIM}   %s${RESET}\n" "$1"; }

printf "${BOLD}Vespra doctor${RESET}\n"
printf "${DIM}%s${RESET}\n\n" "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

#─── check 1: .env present and populated ───────────────────────
if [ ! -f "$ENV_FILE" ]; then
    fail "$ENV_FILE not found — run ./scripts/init.sh first"
    echo ""
    printf "${RED}%d pass, %d fail${RESET}\n" "$PASS" "$FAIL"
    exit 1
fi

# shellcheck disable=SC1090
set -a
. "$ENV_FILE"
set +a

required_vars="VESPRA_NETWORK_MODE KEYMASTER_MASTER_PASSWORD KEYMASTER_BEARER_TOKEN VESPRA_REDIS_URL VESPRA_KEYMASTER_URL ANTHROPIC_API_KEY"
missing_vars=""
for v in $required_vars; do
    eval "val=\${$v-}"
    if [ -z "$val" ]; then
        missing_vars="$missing_vars $v"
    fi
done

if [ -n "$missing_vars" ]; then
    fail ".env is missing required vars:$missing_vars"
else
    pass ".env is populated with all required vars"
fi

#─── check 2: docker installed + daemon running ────────────────
if ! command -v docker >/dev/null 2>&1; then
    fail "docker not installed"
    DOCKER_OK=0
elif ! docker info >/dev/null 2>&1; then
    fail "docker daemon not reachable"
    DOCKER_OK=0
else
    pass "docker installed and daemon running"
    DOCKER_OK=1
fi

#─── check 3: stack health (only if docker compose ps shows it) ─
STACK_UP=0
if [ "$DOCKER_OK" = "1" ]; then
    if docker compose ps --status running 2>/dev/null | grep -q .; then
        STACK_UP=1
    fi
fi

check_http_endpoint() {
    local name="$1"
    local url="$2"
    local match_code="$3"  # expected HTTP code (200 default)
    if ! command -v curl >/dev/null 2>&1; then
        fail "curl not installed — cannot probe $name"
        return
    fi
    local code
    code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$url" || echo "000")"
    if [ "$code" = "$match_code" ]; then
        pass "$name responds ($code at $url)"
    else
        fail "$name did not respond (got $code at $url)"
    fi
}

check_redis() {
    # Redis speaks RESP, not HTTP — probe the TCP port directly via bash's
    # /dev/tcp. If opening the socket succeeds the daemon is accepting
    # connections; the actual PING is handled by the in-container healthcheck.
    if (: </dev/tcp/localhost/6379) 2>/dev/null; then
        pass "redis accepting TCP connections on :6379"
    else
        fail "redis not reachable on :6379"
    fi
}

if [ "$STACK_UP" = "1" ]; then
    check_redis
    check_http_endpoint "keymaster /health" "http://localhost:9100/health" "200"
    check_http_endpoint "gateway /health"   "http://localhost:9001/health" "200"
    check_http_endpoint "dashboard"         "http://localhost:3000/"       "200"
else
    info "stack is not running — skipping service health checks"
    info "start it with: make up"
fi

#─── check 4: at least one chain RPC URL reachable ─────────────
check_rpc() {
    local name="$1"
    local url="$2"
    [ -z "$url" ] && return 1
    if ! command -v curl >/dev/null 2>&1; then
        return 1
    fi
    local resp
    resp="$(curl -s --max-time 5 -X POST -H 'Content-Type: application/json' \
        --data '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$url" 2>/dev/null || true)"
    if printf '%s' "$resp" | grep -q '"result"'; then
        pass "RPC reachable: $name"
        return 0
    else
        return 1
    fi
}

rpc_ok=0
for pair in \
    "BASE_SEPOLIA:${RPC_URL_BASE_SEPOLIA:-}" \
    "ARBITRUM_SEPOLIA:${RPC_URL_ARBITRUM_SEPOLIA:-}" \
    "BASE:${RPC_URL_BASE:-}" \
    "ARBITRUM:${RPC_URL_ARBITRUM:-}"
do
    name="${pair%%:*}"
    url="${pair#*:}"
    if check_rpc "$name" "$url"; then
        rpc_ok=1
    fi
done
if [ "$rpc_ok" = "0" ]; then
    fail "no chain RPC URL responded — all eth_blockNumber probes failed"
fi

#─── checks 5/6/7: burner wallets (requires keymaster up) ──────
if [ "$STACK_UP" = "1" ] && command -v curl >/dev/null 2>&1; then
    wallets_json="$(curl -s --max-time 5 http://localhost:9100/wallets 2>/dev/null || true)"
    if [ -z "$wallets_json" ]; then
        fail "could not query keymaster /wallets"
    else
        # Count wallet entries. Use jq if present, otherwise a coarse grep.
        if command -v jq >/dev/null 2>&1; then
            wallet_count="$(printf '%s' "$wallets_json" | jq 'if type == "array" then length else 0 end' 2>/dev/null || echo 0)"
        else
            wallet_count="$(printf '%s' "$wallets_json" | grep -o '"wallet_id"' | wc -l | tr -d ' ')"
        fi

        if [ "${wallet_count:-0}" = "0" ]; then
            fail "no burner wallets found"
            info "create one with:"
            info "  curl -X POST http://localhost:9100/wallets \\"
            info "    -H \"Authorization: Bearer \$KEYMASTER_BEARER_TOKEN\" \\"
            info "    -H 'Content-Type: application/json' \\"
            info "    -d '{\"chain\":\"base_sepolia\",\"label\":\"my-first-wallet\"}'"
        else
            pass "burner wallets present: $wallet_count"

            # Best-effort balance report (non-blocking).
            if command -v jq >/dev/null 2>&1; then
                empty_count=0
                addrs="$(printf '%s' "$wallets_json" | jq -r '.[] | "\(.chain)|\(.address)|\(.label // "")"' 2>/dev/null || true)"
                while IFS= read -r line; do
                    [ -z "$line" ] && continue
                    chain="$(printf '%s' "$line" | cut -d'|' -f1)"
                    addr="$(printf '%s'  "$line" | cut -d'|' -f2)"
                    label="$(printf '%s' "$line" | cut -d'|' -f3)"
                    bal_json="$(curl -s --max-time 5 "http://localhost:9100/balance/${chain}/${addr}" 2>/dev/null || true)"
                    bal="$(printf '%s' "$bal_json" | jq -r '.balance_eth // 0' 2>/dev/null || echo 0)"
                    if [ "$bal" = "0" ] || [ "$bal" = "0.0" ]; then
                        empty_count=$((empty_count + 1))
                        info "wallet ${label:-$addr} on $chain is empty (0 ETH)"
                    else
                        info "wallet ${label:-$addr} on $chain: $bal ETH"
                    fi
                done <<EOF
$addrs
EOF
                if [ "$empty_count" -gt 0 ]; then
                    info "$empty_count wallet(s) have zero balance — fund them before running a goal."
                fi
            else
                info "install jq for per-wallet balance reporting"
            fi
        fi
    fi
fi

#─── summary ───────────────────────────────────────────────────
echo ""
if [ "$FAIL" = "0" ]; then
    printf "${GREEN}${BOLD}%d pass, %d fail${RESET}\n" "$PASS" "$FAIL"
    exit 0
else
    printf "${RED}${BOLD}%d pass, %d fail${RESET}\n" "$PASS" "$FAIL"
    exit 1
fi
