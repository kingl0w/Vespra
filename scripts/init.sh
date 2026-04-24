#!/usr/bin/env bash
# First-run setup for Vespra. Generates secrets, prompts for required config,
# and writes a working .env. Safe to re-run — it will confirm before overwriting.
set -eu

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

ENV_FILE=".env"
ENV_EXAMPLE=".env.example"

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
dim()  { printf '\033[2m%s\033[0m\n' "$1"; }
err()  { printf '\033[31mERROR:\033[0m %s\n' "$1" >&2; }
ok()   { printf '\033[32m%s\033[0m\n' "$1"; }

if [ ! -f "$ENV_EXAMPLE" ]; then
    err "$ENV_EXAMPLE not found — run this from the repo root."
    exit 1
fi

if ! command -v openssl >/dev/null 2>&1; then
    err "openssl is required but not installed."
    exit 1
fi

#─── step 1: overwrite guard ───────────────────────────────────
if [ -f "$ENV_FILE" ]; then
    bold "$ENV_FILE already exists."
    printf "Overwrite? [y/N]: "
    read -r reply
    case "$reply" in
        [yY]|[yY][eE][sS]) ;;
        *) echo "Aborting — existing $ENV_FILE preserved."; exit 0 ;;
    esac
fi

#─── step 2: copy template ─────────────────────────────────────
cp "$ENV_EXAMPLE" "$ENV_FILE"
ok "Copied $ENV_EXAMPLE → $ENV_FILE"

#─── step 3/4: generate secrets ────────────────────────────────
gen_secret() {
    # 32 chars, alphanumeric. openssl rand -base64 24 is 32 chars but may
    # contain +/= — use a larger pool and strip to alphanumeric.
    openssl rand -base64 48 | tr -dc 'A-Za-z0-9' | head -c 32
}

MASTER_PASSWORD="$(gen_secret)"
BEARER_TOKEN="$(gen_secret)"

if [ -z "$MASTER_PASSWORD" ] || [ -z "$BEARER_TOKEN" ]; then
    err "Failed to generate secrets — openssl output was empty."
    exit 1
fi

#─── step 5: write secrets into .env ───────────────────────────
# sed -i behaves differently on GNU vs BSD; use a portable in-place pattern.
sed_inplace() {
    # $1 = sed expression, $2 = file
    if sed --version >/dev/null 2>&1; then
        sed -i "$1" "$2"
    else
        sed -i '' "$1" "$2"
    fi
}

set_env_var() {
    # Replaces the full line "KEY=..." with "KEY=<value>" (no comment suffix).
    # Uses | as sed separator so URLs don't need escaping.
    local key="$1"
    local value="$2"
    local escaped
    # Escape sed replacement special chars: & \ |
    escaped="$(printf '%s' "$value" | sed 's/[&\\|]/\\&/g')"
    sed_inplace "s|^${key}=.*|${key}=${escaped}|" "$ENV_FILE"
}

uncomment_env_var() {
    # Turns "# KEY=..." into "KEY=<value>".
    local key="$1"
    local value="$2"
    local escaped
    escaped="$(printf '%s' "$value" | sed 's/[&\\|]/\\&/g')"
    sed_inplace "s|^# *${key}=.*|${key}=${escaped}|" "$ENV_FILE"
}

set_env_var "KEYMASTER_MASTER_PASSWORD" "$MASTER_PASSWORD"
set_env_var "KEYMASTER_BEARER_TOKEN"    "$BEARER_TOKEN"
ok "Generated KEYMASTER_MASTER_PASSWORD and KEYMASTER_BEARER_TOKEN"

#─── step 6: NETWORK_MODE prompt ───────────────────────────────
bold ""
bold "Network mode:"
dim "  testnet — relaxed risk gates, synthetic fallback pools (safe default)"
dim "  mainnet — strict risk gates, real pool data, real funds"
printf "Network mode [testnet]: "
read -r net
case "$(printf '%s' "$net" | tr '[:upper:]' '[:lower:]')" in
    mainnet) NETWORK_MODE="mainnet" ;;
    *)       NETWORK_MODE="testnet" ;;
esac
set_env_var "VESPRA_NETWORK_MODE" "$NETWORK_MODE"
ok "NETWORK_MODE=$NETWORK_MODE"

#─── step 7: RPC URLs ──────────────────────────────────────────
if [ "$NETWORK_MODE" = "mainnet" ]; then
    bold ""
    bold "Mainnet RPC URLs (required — leave blank to skip a chain):"
    printf "RPC_URL_BASE [blank]: "
    read -r base
    printf "RPC_URL_ARBITRUM [blank]: "
    read -r arb
    if [ -n "$base" ]; then
        uncomment_env_var "RPC_URL_BASE" "$base"
        ok "RPC_URL_BASE set"
    fi
    if [ -n "$arb" ]; then
        uncomment_env_var "RPC_URL_ARBITRUM" "$arb"
        ok "RPC_URL_ARBITRUM set"
    fi
else
    dim "Keeping testnet RPC defaults (Base Sepolia + Arbitrum Sepolia)."
fi

#─── step 8: ANTHROPIC_API_KEY (required, hidden input) ────────
bold ""
bold "Anthropic API key (required):"
dim "  Input is hidden. Get one at https://console.anthropic.com"
while :; do
    printf "ANTHROPIC_API_KEY: "
    stty -echo 2>/dev/null || true
    read -r api_key
    stty echo 2>/dev/null || true
    echo ""
    if [ -n "$api_key" ]; then
        break
    fi
    err "required — please paste a key."
done
set_env_var "ANTHROPIC_API_KEY" "$api_key"
ok "ANTHROPIC_API_KEY set"

#─── step 9: optional Telegram ─────────────────────────────────
bold ""
bold "Telegram notifications (optional — Enter to skip):"
printf "Bot token [skip]: "
read -r tg_token
if [ -n "$tg_token" ]; then
    uncomment_env_var "VESPRA_TELEGRAM_BOT_TOKEN" "$tg_token"
    printf "Chat ID [skip]: "
    read -r tg_chat
    if [ -n "$tg_chat" ]; then
        uncomment_env_var "VESPRA_TELEGRAM_CHAT_ID" "$tg_chat"
        ok "Telegram configured"
    else
        dim "Chat ID not set — Telegram will not be fully configured."
    fi
else
    dim "Skipped Telegram."
fi

#─── step 10: validate ─────────────────────────────────────────
bold ""
bold "Validating $ENV_FILE…"

# shellcheck disable=SC1090
set -a
. "$ENV_FILE"
set +a

missing=0
check_var() {
    local name="$1"
    eval "val=\${$name-}"
    if [ -z "$val" ] || [ "$val" = "change-me-to-something-long-and-random" ]; then
        err "$name is empty"
        missing=$((missing + 1))
    fi
}

check_var VESPRA_NETWORK_MODE
check_var KEYMASTER_MASTER_PASSWORD
check_var KEYMASTER_BEARER_TOKEN
check_var VESPRA_REDIS_URL
check_var VESPRA_KEYMASTER_URL
check_var ANTHROPIC_API_KEY

if [ "$missing" -gt 0 ]; then
    err "$missing required var(s) missing — re-run this script."
    exit 1
fi

#─── step 11: success ──────────────────────────────────────────
echo ""
ok "Vespra is configured. Run \`make up\` to start."
