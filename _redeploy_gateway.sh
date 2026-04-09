#!/usr/bin/env bash
set -euo pipefail
echo "[1/3] stop gateway"
systemctl stop vespra-gateway-rs
echo "[2/3] start gateway"
systemctl start vespra-gateway-rs
sleep 4
echo "[3/3] status + mem"
systemctl status vespra-gateway-rs --no-pager | head -10
echo "---"
ps aux | grep -E '[g]ateway-rs'
echo "---"
curl -s -w "\nHTTP %{http_code}\n" http://127.0.0.1:9001/health
echo "---"
echo "[4/4] cleanup target/debug"
rm -rf /home/taddymason/vespra/gateway-rs/target/debug
echo "done"
