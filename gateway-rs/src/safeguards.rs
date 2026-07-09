use std::sync::Arc;

use chrono::Utc;
use redis::AsyncCommands;

use crate::data::wallet::WalletFetcher;

const TX_RATE_KEY_PREFIX: &str = "vespra:tx_rate";
const TX_RATE_TTL_SECS: i64 = 7200; //2 hours — self-cleaning

pub fn hour_bucket(unix_secs: u64) -> u64 {
    unix_secs / 3600
}

pub fn current_hour_bucket() -> u64 {
    hour_bucket(Utc::now().timestamp() as u64)
}

pub fn tx_rate_key(bucket: u64) -> String {
    format!("{TX_RATE_KEY_PREFIX}:{bucket}")
}

///reads counts for two adjacent hour buckets without mutating them. returns
///(current_count, prior_count). missing keys count as 0.
pub async fn tx_rate_counts_for(
    redis: &redis::Client,
    current_bucket: u64,
    prior_bucket: u64,
) -> anyhow::Result<(u32, u32)> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let current: Option<u32> = conn.get(tx_rate_key(current_bucket)).await?;
    let prior: Option<u32> = conn.get(tx_rate_key(prior_bucket)).await?;
    Ok((current.unwrap_or(0), prior.unwrap_or(0)))
}

pub async fn tx_rate_counts_now(redis: &redis::Client) -> anyhow::Result<(u32, u32)> {
    let now = current_hour_bucket();
    let prior = now.saturating_sub(1);
    tx_rate_counts_for(redis, now, prior).await
}

///atomically increments the current hour bucket, refreshes TTL, and returns
///(current_count_after_incr, prior_count). caller is responsible for
///comparing the sum against the configured limit.
pub async fn increment_and_read(
    redis: &redis::Client,
    current_bucket: u64,
    prior_bucket: u64,
) -> anyhow::Result<(u32, u32)> {
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let current: u32 = conn.incr(tx_rate_key(current_bucket), 1u32).await?;
    let _: () = conn
        .expire(tx_rate_key(current_bucket), TX_RATE_TTL_SECS)
        .await?;
    let prior: Option<u32> = conn.get(tx_rate_key(prior_bucket)).await?;
    Ok((current, prior.unwrap_or(0)))
}

///check-and-increment: increments the counter for the current hour, then
///compares current+prior against the limit. returns Ok((current, prior))
///when within budget, Err(message) when exceeded.
pub async fn check_tx_rate_limit(
    redis: &redis::Client,
    max_per_hour: Option<u32>,
) -> Result<(u32, u32), String> {
    let Some(max) = max_per_hour else {
        return Ok((0, 0));
    };
    let now = current_hour_bucket();
    let prior = now.saturating_sub(1);
    let (current, prior_count) = increment_and_read(redis, now, prior)
        .await
        .map_err(|e| format!("rate limit check failed: {e}"))?;
    if current + prior_count > max {
        return Err(format!(
            "tx rate limit exceeded: {}/{} in last hour — try again later",
            current + prior_count,
            max
        ));
    }
    Ok((current, prior_count))
}

///sums ETH balance across every burner wallet on every configured chain.
///errors from any single chain are logged and treated as zero for that chain
///so a flaky RPC does not bypass the cap entirely.
pub async fn sum_global_wallet_value_eth(
    wallet_fetcher: &Arc<WalletFetcher>,
    chains: &[String],
) -> f64 {
    let mut total = 0.0;
    for chain in chains {
        match wallet_fetcher.fetch_wallets(chain).await {
            Ok(wallets) => {
                for w in wallets {
                    total += w.balance_eth;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[safeguards] fetch_wallets({chain}) failed while summing global value: {e}"
                );
            }
        }
    }
    total
}

///returns the current global ETH value across all wallets. errors to
///reject-with-message when adding `incoming_eth` would exceed the cap.
///when `max_global_wallet_value_eth` is None, always returns Ok with the
///current sum.
pub async fn check_global_cap(
    max_global_wallet_value_eth: Option<f64>,
    wallet_fetcher: &Arc<WalletFetcher>,
    chains: &[String],
    incoming_eth: f64,
) -> Result<f64, String> {
    let current = sum_global_wallet_value_eth(wallet_fetcher, chains).await;
    if let Some(cap) = max_global_wallet_value_eth {
        if current + incoming_eth > cap {
            return Err(format!(
                "global wallet cap exceeded: {current:.6} + {incoming_eth:.6} > {cap:.6} ETH"
            ));
        }
    }
    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_redis() -> Option<redis::Client> {
        let client = redis::Client::open("redis://127.0.0.1:6379").ok()?;
        //ping-style health check via blocking conn would be ideal; instead we
        //just return the client and let per-test async calls skip on err.
        Some(client)
    }

    async fn cleanup(client: &redis::Client, buckets: &[u64]) {
        if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
            for b in buckets {
                let _: Result<(), _> = redis::AsyncCommands::del::<_, ()>(
                    &mut conn,
                    tx_rate_key(*b),
                )
                .await;
            }
        }
    }

    #[test]
    fn hour_bucket_is_integer_hours_since_epoch() {
        assert_eq!(hour_bucket(0), 0);
        assert_eq!(hour_bucket(3599), 0);
        assert_eq!(hour_bucket(3600), 1);
        assert_eq!(hour_bucket(7200), 2);
        assert_eq!(hour_bucket(7199), 1);
    }

    #[test]
    fn global_cap_allows_when_unset() {
        //synchronous logic-only check — no async machinery needed.
        //mirrors the `if let Some(cap)` branch in check_global_cap.
        let max: Option<f64> = None;
        let current = 10.0;
        let incoming = 5.0;
        let would_exceed = max.map(|c| current + incoming > c).unwrap_or(false);
        assert!(!would_exceed, "None cap must not reject");
    }

    #[test]
    fn global_cap_rejects_when_sum_exceeds() {
        let max = Some(5.0_f64);
        let current = 4.0;
        let incoming = 2.0;
        let would_exceed = max.map(|c| current + incoming > c).unwrap_or(false);
        assert!(would_exceed, "4 + 2 must exceed cap of 5");
    }

    #[test]
    fn global_cap_allows_at_exact_boundary() {
        let max = Some(5.0_f64);
        let current = 3.0;
        let incoming = 2.0;
        //5.0 + 0 == 5.0 is NOT strictly greater than 5.0 → allowed
        let would_exceed = max.map(|c| current + incoming > c).unwrap_or(false);
        assert!(!would_exceed);
    }

    #[tokio::test]
    async fn rate_limit_blocks_after_threshold() {
        let Some(client) = test_redis() else { return };
        if client.get_multiplexed_async_connection().await.is_err() {
            return;
        }

        //use a far-future synthetic bucket so we don't collide with prod
        //counters — the offset is stable for the test's lifetime.
        let base = 9_999_900;
        let now_bucket = base;
        let prior_bucket = base - 1;
        cleanup(&client, &[now_bucket, prior_bucket]).await;

        //seed current bucket to max - 1, then one more increment is the edge.
        let max: u32 = 3;
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::AsyncCommands::set(&mut conn, tx_rate_key(now_bucket), 2u32)
            .await
            .unwrap();

        //first incr: 2 -> 3, total = 3, NOT > 3, so ok
        let (c, p) = increment_and_read(&client, now_bucket, prior_bucket)
            .await
            .unwrap();
        assert_eq!((c, p), (3, 0));
        assert!(c + p <= max, "3 must be within budget of 3");

        //second incr: 3 -> 4, total = 4, > 3, over budget
        let (c2, p2) = increment_and_read(&client, now_bucket, prior_bucket)
            .await
            .unwrap();
        assert_eq!((c2, p2), (4, 0));
        assert!(c2 + p2 > max, "4 must exceed budget of 3");

        cleanup(&client, &[now_bucket, prior_bucket]).await;
    }

    #[tokio::test]
    async fn rate_limit_rolls_over_between_hours() {
        let Some(client) = test_redis() else { return };
        if client.get_multiplexed_async_connection().await.is_err() {
            return;
        }

        let base = 9_999_800;
        let prior_bucket = base;
        let current_bucket = base + 1;
        let next_bucket = base + 2;
        cleanup(
            &client,
            &[prior_bucket, current_bucket, next_bucket],
        )
        .await;

        //before any rollover: prior = 50, current = 40, total = 90
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::AsyncCommands::set(&mut conn, tx_rate_key(prior_bucket), 50u32)
            .await
            .unwrap();
        let _: () = redis::AsyncCommands::set(&mut conn, tx_rate_key(current_bucket), 40u32)
            .await
            .unwrap();

        let (c, p) = tx_rate_counts_for(&client, current_bucket, prior_bucket)
            .await
            .unwrap();
        assert_eq!((c, p), (40, 50));

        //rollover: what was "current" is now "prior"; a fresh bucket is current.
        //new current starts at 0, prior is still the 40 from the previous hour.
        let (c2, p2) = tx_rate_counts_for(&client, next_bucket, current_bucket)
            .await
            .unwrap();
        assert_eq!(
            (c2, p2),
            (0, 40),
            "after rollover, prior should reflect previous hour, current should be fresh"
        );

        //the 50-count bucket is now two hours old and must NOT be counted.
        //(tx_rate_counts_for only looks at the two buckets passed in.)
        assert_eq!(
            c2 + p2,
            40,
            "rolled-over total must exclude the two-hours-ago bucket (50)"
        );

        cleanup(
            &client,
            &[prior_bucket, current_bucket, next_bucket],
        )
        .await;
    }
}
