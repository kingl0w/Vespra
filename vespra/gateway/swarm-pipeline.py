#!/usr/bin/env python3
"""
Vespra Swarm Pipeline — Parallel agents, tight output constraints.
Scout + Sentinel run in parallel → Risk evaluates → Coordinator summarizes.
"""

import json, urllib.request, sys, logging, os
from datetime import datetime, timezone
from concurrent.futures import ThreadPoolExecutor, as_completed

GATEWAY = "http://127.0.0.1:9000"

logging.basicConfig(level=logging.INFO, format="%(asctime)s [%(levelname)s] %(message)s", datefmt="%Y-%m-%d %H:%M:%S")
log = logging.getLogger("pipeline")


def call_agent(agent, message):
    url = f"{GATEWAY}/agent/{agent}"
    payload = json.dumps({"message": message}).encode()
    req = urllib.request.Request(url, data=payload, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=180) as resp:
            data = json.loads(resp.read())
            if data.get("status") == "completed":
                return data["response"]
            log.error(f"{agent} failed: {data.get('error', 'unknown')}")
            return None
    except Exception as e:
        log.error(f"{agent} error: {e}")
        return None


def run_pipeline():
    log.info("=== Vespra Swarm Pipeline Started ===")
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    # --- Phase 1: Scout + Sentinel in parallel ---
    log.info("[1/3] Scout + Sentinel running in parallel...")

    scout_prompt = f"""Scan DeFi yield opportunities across Ethereum, Base, Arbitrum, Optimism.
Return ONLY a JSON array, no commentary. Max 5 opportunities. Each object:
{{"protocol":"","chain":"","type":"stablecoin|lp|farm","apy":0,"tvl":0,"risk_notes":"","url":""}}
Thresholds: stablecoin >8% APY, LP >25% APY, new pools >$500k TVL.
Use training knowledge. Be concise. Timestamp: {ts}"""

    sentinel_prompt = f"""Check health of major DeFi positions on Ethereum, Base, Arbitrum.
Return ONLY a JSON array, no commentary. Max 5 alerts. Each object:
{{"protocol":"","position":"","alert_type":"","severity":"LOW|MEDIUM|HIGH|CRITICAL","details":"","recommended_action":""}}
Focus on: health factors <1.5, depegs >2%, TVL drops >15%/1h, contract upgrades.
Use training knowledge. Be concise. Timestamp: {ts}"""

    scout_result = None
    sentinel_result = None

    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = {
            pool.submit(call_agent, "scout", scout_prompt): "scout",
            pool.submit(call_agent, "sentinel", sentinel_prompt): "sentinel",
        }
        for future in as_completed(futures):
            name = futures[future]
            result = future.result()
            if name == "scout":
                scout_result = result
                log.info(f"Scout returned {len(result) if result else 0} chars")
            else:
                sentinel_result = result
                log.info(f"Sentinel returned {len(result) if result else 0} chars")

    if not scout_result:
        log.error("Scout failed. Aborting pipeline.")
        return False

    # --- Phase 2: Risk evaluates Scout findings ---
    log.info("[2/3] Risk evaluating opportunities...")

    risk_prompt = f"""Evaluate these DeFi opportunities. Return ONLY a JSON array, no commentary. Max 5 assessments. Each object:
{{"protocol":"","chain":"","score":"LOW|MEDIUM|HIGH|CRITICAL","factors":[{{"category":"","rating":"","detail":""}}],"recommendation":""}}
Keep each recommendation under 20 words. Be conservative.

Scout findings:
{scout_result}"""

    risk_result = call_agent("risk", risk_prompt)
    if not risk_result:
        log.error("Risk failed. Passing Scout data through.")
        risk_result = '[]'
    log.info(f"Risk returned {len(risk_result)} chars")

    # --- Phase 3: Coordinator synthesizes ---
    log.info("[3/3] Coordinator synthesizing report...")

    sentinel_section = f"\nSENTINEL ALERTS:\n{sentinel_result}" if sentinel_result else ""

    coordinator_prompt = f"""Synthesize into a Telegram message under 1500 characters.
Format: Top 3 opportunities (risk-adjusted), any active alerts, one-line recommendations.
No markdown headers. Use emoji sparingly. Be direct.

SCOUT DATA:
{scout_result}

RISK SCORES:
{risk_result}
{sentinel_section}
Timestamp: {ts}"""

    coordinator_result = call_agent("coordinator", coordinator_prompt)
    if not coordinator_result:
        log.error("Coordinator failed.")
        return False
    log.info(f"Coordinator returned {len(coordinator_result)} chars")

    # --- Output ---
    print("\n" + "="*60)
    print("VESPRA SWARM REPORT")
    print("="*60)
    print(coordinator_result)
    print("="*60)

    # Save report
    report_file = f"/opt/vespra-gateway/reports/{datetime.now(timezone.utc).strftime('%Y-%m-%d_%H%M')}.txt"
    try:
        os.makedirs("/opt/vespra-gateway/reports", exist_ok=True)
        with open(report_file, 'w') as f:
            f.write(f"Vespra Swarm Report — {ts}\n\n")
            f.write(f"--- SCOUT ---\n{scout_result}\n\n")
            if sentinel_result:
                f.write(f"--- SENTINEL ---\n{sentinel_result}\n\n")
            f.write(f"--- RISK ---\n{risk_result}\n\n")
            f.write(f"--- REPORT ---\n{coordinator_result}\n")
        log.info(f"Report saved: {report_file}")
    except Exception as e:
        log.warning(f"Could not save report: {e}")

    log.info("=== Pipeline Complete ===")
    return True


if __name__ == "__main__":
    success = run_pipeline()
    sys.exit(0 if success else 1)
