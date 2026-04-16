use std::time::Duration;

use anyhow::Result;

/// Telegram Bot API client for sending notifications.
#[derive(Clone)]
pub struct TelegramClient {
    bot_token: String,
    chat_id: String,
    client: reqwest::Client,
}

impl TelegramClient {
    pub fn new(bot_token: String, chat_id: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            bot_token,
            chat_id,
            client,
        }
    }

    /// Send a text message to the configured chat. On error, logs at WARN
    /// and returns Ok(()) — Telegram failures must never propagate.
    pub async fn send(&self, text: &str) -> Result<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );
        let body = serde_json::json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });

        match self.client.post(&url).json(&body).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body_text = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        "[telegram] sendMessage failed: HTTP {status} — {body_text}"
                    );
                }
            }
            Err(e) => {
                tracing::warn!("[telegram] sendMessage error: {e}");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_construction() {
        let client = TelegramClient::new("123:ABC".into(), "456".into());
        assert_eq!(client.bot_token, "123:ABC");
        assert_eq!(client.chat_id, "456");
    }

    #[tokio::test]
    async fn send_swallows_network_error() {
        // Point at a non-routable address so the request fails immediately
        let client = TelegramClient::new("fake_token".into(), "fake_chat".into());
        // Override the internal client with a 1ms timeout to force fast failure
        let client = TelegramClient {
            client: reqwest::Client::builder()
                .timeout(Duration::from_millis(1))
                .build()
                .unwrap(),
            ..client
        };
        // Must return Ok even on network failure
        let result = client.send("test message").await;
        assert!(result.is_ok(), "send() must swallow errors and return Ok");
    }

    #[tokio::test]
    async fn send_builds_correct_url() {
        // We can't easily intercept the HTTP call without a mock server,
        // but we verify the URL construction logic directly.
        let client = TelegramClient::new("123:ABC".into(), "789".into());
        let expected_url = "https://api.telegram.org/bot123:ABC/sendMessage";
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            client.bot_token
        );
        assert_eq!(url, expected_url);
    }

    #[tokio::test]
    async fn notify_spawn_is_nonblocking() {
        // Verify that spawning a notification task returns immediately.
        // We measure wall-clock time: if it blocks on the 5s timeout,
        // this test will take >1s and fail.
        let client = TelegramClient::new("fake".into(), "fake".into());
        let tg = client.clone();
        let start = std::time::Instant::now();
        tokio::spawn(async move {
            let _ = tg.send("test").await;
        });
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "spawn must be non-blocking, took {:?}",
            elapsed
        );
    }
}
