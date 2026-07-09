#!/usr/bin/env bash
# Unit test for scripts/doctor.sh — verifies the .env-level checks across every
# supported LLM provider. We don't need a running stack for these; we only
# exercise the parts of doctor.sh that read .env and report LLM configuration.
#
# For each provider we build a throwaway .env in a tmpdir, invoke doctor.sh with
# the tmpdir as $REPO_ROOT (doctor.sh is written to accept that via the cd at
# the top), and grep the output. Exit 0 if all assertions hold, 1 otherwise.
set -u

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DOCTOR="$REPO_ROOT/scripts/doctor.sh"

if [ ! -x "$DOCTOR" ]; then
    echo "doctor.sh not found or not executable at $DOCTOR" >&2
    exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Minimal set of vars required by doctor.sh outside the LLM block. Everything
# related to the stack being up (docker, curl checks) will fail gracefully —
# we grep only for the LLM-specific lines we're asserting on.
write_env() {
    local dir="$1"
    local provider="$2"
    local model="$3"
    local api_key="$4"      # may be empty
    local extra="${5-}"     # optional extra lines (e.g. ANTHROPIC_API_KEY for deprecation test)

    cat > "$dir/.env" <<EOF
VESPRA_NETWORK_MODE=testnet
KEYMASTER_MASTER_PASSWORD=$(printf 'x%.0s' $(seq 1 32))
KEYMASTER_BEARER_TOKEN=$(printf 'y%.0s' $(seq 1 32))
VESPRA_REDIS_URL=redis://redis:6379
VESPRA_KEYMASTER_URL=http://keymaster:9100
VESPRA_LLM_PROVIDER=$provider
VESPRA_LLM_MODEL=$model
VESPRA_LLM_API_KEY=$api_key
RPC_URL_BASE_SEPOLIA=https://sepolia.base.org
RPC_URL_ARBITRUM_SEPOLIA=https://sepolia-rollup.arbitrum.io/rpc
$extra
EOF
}

# doctor.sh cd's to its own repo root via dirname-of-$0/.. — for the test we
# need a copy living inside the tmpdir so that repo-root is the tmpdir.
copy_doctor() {
    local dir="$1"
    mkdir -p "$dir/scripts"
    cp "$DOCTOR" "$dir/scripts/doctor.sh"
    chmod +x "$dir/scripts/doctor.sh"
}

run_doctor() {
    # doctor.sh exits non-zero on any fail; capture stdout + return code.
    local dir="$1"
    (cd "$dir" && ./scripts/doctor.sh 2>&1)
    return $?
}

FAIL=0
assert_contains() {
    local haystack="$1"
    local needle="$2"
    local label="$3"
    if printf '%s' "$haystack" | grep -q -- "$needle"; then
        printf '  ✅ %s\n' "$label"
    else
        printf '  ❌ %s — expected to find: %s\n' "$label" "$needle"
        FAIL=$((FAIL + 1))
    fi
}
assert_not_contains() {
    local haystack="$1"
    local needle="$2"
    local label="$3"
    if printf '%s' "$haystack" | grep -q -- "$needle"; then
        printf '  ❌ %s — unexpected: %s\n' "$label" "$needle"
        FAIL=$((FAIL + 1))
    else
        printf '  ✅ %s\n' "$label"
    fi
}

# ─── test matrix ───────────────────────────────────────────────

echo "→ provider=anthropic with key"
DIR="$TMP/anthropic"
mkdir -p "$DIR"
copy_doctor "$DIR"
write_env "$DIR" "anthropic" "claude-sonnet-4-6" "sk-ant-test"
OUT="$(run_doctor "$DIR" || true)"
assert_contains "$OUT" "LLM configured — provider=anthropic, model=claude-sonnet-4-6" "reports LLM configured"
assert_not_contains "$OUT" "VESPRA_LLM_API_KEY(for provider=anthropic)" "no missing-key error"

echo "→ provider=deepseek with key"
DIR="$TMP/deepseek"
mkdir -p "$DIR"
copy_doctor "$DIR"
write_env "$DIR" "deepseek" "deepseek-chat" "sk-ds-test"
OUT="$(run_doctor "$DIR" || true)"
assert_contains "$OUT" "LLM configured — provider=deepseek, model=deepseek-chat" "reports LLM configured"

echo "→ provider=ollama without key (allowed)"
DIR="$TMP/ollama"
mkdir -p "$DIR"
copy_doctor "$DIR"
write_env "$DIR" "ollama" "llama3.1:8b" ""
OUT="$(run_doctor "$DIR" || true)"
assert_contains "$OUT" "LLM configured — provider=ollama" "ollama passes without key"
assert_not_contains "$OUT" "VESPRA_LLM_API_KEY(for provider=ollama)" "ollama doesn't complain about key"

echo "→ provider=openai with MISSING key (should fail)"
DIR="$TMP/openai-nokey"
mkdir -p "$DIR"
copy_doctor "$DIR"
write_env "$DIR" "openai" "gpt-4o" ""
OUT="$(run_doctor "$DIR" || true)"
assert_contains "$OUT" "VESPRA_LLM_API_KEY(for provider=openai)" "reports missing key for openai"

echo "→ legacy ANTHROPIC_API_KEY set — deprecation warning"
DIR="$TMP/legacy"
mkdir -p "$DIR"
copy_doctor "$DIR"
write_env "$DIR" "anthropic" "claude-sonnet-4-6" "sk-ant-new" "ANTHROPIC_API_KEY=sk-ant-old"
OUT="$(run_doctor "$DIR" || true)"
assert_contains "$OUT" "ANTHROPIC_API_KEY is deprecated" "warns on deprecated var"

echo ""
if [ "$FAIL" = "0" ]; then
    echo "all doctor_test assertions passed"
    exit 0
else
    echo "$FAIL doctor_test assertion(s) failed"
    exit 1
fi
