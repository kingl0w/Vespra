#!/usr/bin/env bash
set -euo pipefail

TOKEN=$(grep VESPRA_KM_AUTH_TOKEN /opt/vespra-keymaster/.env | cut -d= -f2-)
echo "[1/3] km token len=${#TOKEN}"

echo "[2/3] PUT /wallets/.../cap cap_eth=0.1"
curl -s -w "\nHTTP %{http_code}\n" -X PUT \
  http://127.0.0.1:9100/wallets/7cb4bdd4-cdc8-4b0b-ac8f-ef83f31e739e/cap \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"cap_eth": "0.1"}'

echo "[3/3] restart gateway"
systemctl restart vespra-gateway-rs
sleep 3
curl -s -w "\nHTTP %{http_code}\n" http://127.0.0.1:9001/health
