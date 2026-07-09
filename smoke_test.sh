#!/usr/bin/env bash
set -euo pipefail

API="http://127.0.0.1:9001"
WALLET="base-test-1"
TIMEOUT_SECS=480  # 8 minutes

submit_goal() {
    local raw_goal="$1"
    local label="$2"
    local resp
    resp=$(curl -s -w "\n%{http_code}" -X POST "$API/goals" \
        -H "Content-Type: application/json" \
        -d "{\"raw_goal\": \"$raw_goal\", \"wallet_label\": \"$label\"}")
    local http_code
    http_code=$(echo "$resp" | tail -1)
    local body
    body=$(echo "$resp" | sed '$d')
    echo "HTTP $http_code"
    echo "$body"
}

poll_goal() {
    local goal_id="$1"
    local label="$2"
    local start_ts
    start_ts=$(date +%s)
    local last_step=""

    while true; do
        local now
        now=$(date +%s)
        local elapsed=$(( now - start_ts ))
        if [ "$elapsed" -ge "$TIMEOUT_SECS" ]; then
            echo "[$(date -u +%H:%M:%S)] TIMED OUT after ${elapsed}s"
            return 1
        fi

        local resp
        resp=$(curl -s "$API/goals/$goal_id")
        local status
        status=$(echo "$resp" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])" 2>/dev/null || echo "unknown")
        local step
        step=$(echo "$resp" | python3 -c "import sys,json; print(json.load(sys.stdin)['current_step'])" 2>/dev/null || echo "unknown")
        local error
        error=$(echo "$resp" | python3 -c "import sys,json; e=json.load(sys.stdin).get('error'); print(e if e else '')" 2>/dev/null || echo "")

        if [ "$step" != "$last_step" ]; then
            echo "[$(date -u +%H:%M:%S)] step=$step  status=$status  (${elapsed}s)"
            last_step="$step"
        fi

        case "$status" in
            completed|failed|cancelled)
                echo "[$(date -u +%H:%M:%S)] TERMINAL: status=$status step=$step (${elapsed}s)"
                if [ -n "$error" ]; then
                    echo "  error: $error"
                fi
                # Print full final state
                echo "$resp" | python3 -m json.tool 2>/dev/null
                return 0
                ;;
        esac

        sleep 5
    done
}

echo "========================================"
echo "  PHASE 1: Strategy Smoke Tests"
echo "========================================"
echo ""

# Check gateway logs for strategy classification
check_strategy_log() {
    local goal_id="$1"
    journalctl -u vespra-gateway-rs --no-pager --since "5 minutes ago" 2>/dev/null \
        | grep -i "$goal_id" | grep -iE "strategy|classif|keyword|llm" | tail -5 \
        || echo "  (no strategy classification log lines found for $goal_id)"
}

# --- Test 1: yield_rotate ---
echo "--- Test 1: yield_rotate ---"
echo "Submitting: Earn yield on 0.001 ETH on Base Sepolia, WETH USDC only"
RESP=$(submit_goal "Earn yield on 0.001 ETH on Base Sepolia, WETH USDC only" "$WALLET")
echo "$RESP"
HTTP_CODE=$(echo "$RESP" | head -1)
BODY=$(echo "$RESP" | tail -n +2)

GOAL_ID=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])" 2>/dev/null || echo "")
if [ -z "$GOAL_ID" ]; then
    echo "FAILED to submit goal"
else
    echo "goal_id=$GOAL_ID"
    poll_goal "$GOAL_ID" "yield_rotate"
    echo ""
    echo "Strategy classification log:"
    check_strategy_log "$GOAL_ID"
fi
echo ""

# Cancel if still running
if [ -n "$GOAL_ID" ]; then
    curl -s -X POST "$API/goals/$GOAL_ID/cancel" > /dev/null 2>&1 || true
fi

echo ""
echo "--- Test 2: compound ---"
echo "Submitting: Compound 0.001 ETH on Base Sepolia"
RESP=$(submit_goal "Compound 0.001 ETH on Base Sepolia" "$WALLET")
echo "$RESP"
BODY=$(echo "$RESP" | tail -n +2)

GOAL_ID2=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])" 2>/dev/null || echo "")
if [ -z "$GOAL_ID2" ]; then
    echo "FAILED to submit goal"
else
    echo "goal_id=$GOAL_ID2"
    poll_goal "$GOAL_ID2" "compound"
    echo ""
    echo "Strategy classification log:"
    check_strategy_log "$GOAL_ID2"
fi
echo ""

if [ -n "$GOAL_ID2" ]; then
    curl -s -X POST "$API/goals/$GOAL_ID2/cancel" > /dev/null 2>&1 || true
fi

echo ""
echo "--- Test 3: adaptive ---"
echo "Submitting: Grow 0.001 ETH on Base Sepolia"
RESP=$(submit_goal "Grow 0.001 ETH on Base Sepolia" "$WALLET")
echo "$RESP"
BODY=$(echo "$RESP" | tail -n +2)

GOAL_ID3=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin)['id'])" 2>/dev/null || echo "")
if [ -z "$GOAL_ID3" ]; then
    echo "FAILED to submit goal"
else
    echo "goal_id=$GOAL_ID3"
    poll_goal "$GOAL_ID3" "adaptive"
    echo ""
    echo "Strategy classification log:"
    check_strategy_log "$GOAL_ID3"
fi
echo ""

if [ -n "$GOAL_ID3" ]; then
    curl -s -X POST "$API/goals/$GOAL_ID3/cancel" > /dev/null 2>&1 || true
fi

echo ""
echo "========================================"
echo "  PHASE 2: Edge Case Re-test"
echo "========================================"
echo ""

echo "--- Test 4: Unknown chain (Avalanche) ---"
echo "Submitting: Earn yield on 0.001 ETH on Avalanche"
RESP=$(submit_goal "Earn yield on 0.001 ETH on Avalanche" "$WALLET")
echo "$RESP"
echo ""

echo "--- Test 5: Huge amount (500 ETH) ---"
echo "Submitting: Grow 500 ETH on Base Sepolia"
RESP=$(submit_goal "Grow 500 ETH on Base Sepolia" "$WALLET")
echo "$RESP"
echo ""

echo "========================================"
echo "  PHASE 3: NullBoiler Stability"
echo "========================================"
echo ""

echo "Boiler status (T=0):"
systemctl status vespra-boiler --no-pager 2>&1 | head -10
BOILER_PID=$(systemctl show vespra-boiler --property=MainPID --value 2>/dev/null || echo "unknown")
echo "MainPID=$BOILER_PID"
echo ""
echo "Waiting 60 seconds..."
sleep 60
echo ""
echo "Boiler status (T=60s):"
systemctl status vespra-boiler --no-pager 2>&1 | head -10
BOILER_PID2=$(systemctl show vespra-boiler --property=MainPID --value 2>/dev/null || echo "unknown")
echo "MainPID=$BOILER_PID2"

if [ "$BOILER_PID" = "$BOILER_PID2" ]; then
    echo "PASS: PID unchanged ($BOILER_PID) — no restart in 60s"
else
    echo "FAIL: PID changed from $BOILER_PID to $BOILER_PID2 — boiler restarted!"
fi

echo ""
echo "========================================"
echo "  SMOKE TEST COMPLETE"
echo "========================================"
