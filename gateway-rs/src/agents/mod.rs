pub mod chat;
pub mod coordinator;
pub mod executor;
pub mod launcher;
pub mod risk;
pub mod scout;
pub mod sentinel;
pub mod sniper;
pub mod trader;
pub mod yield_agent;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait AgentClient: Send + Sync {
    async fn call(&self, system: &str, task: &str) -> Result<String>;
}

pub struct LlmClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn new(client: reqwest::Client, base_url: String, api_key: String, model: String) -> Self {
        Self { client, base_url, api_key, model }
    }
}

#[async_trait]
impl AgentClient for LlmClient {
    async fn call(&self, system: &str, task: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 2000,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": task },
            ],
        });

        let resp = self.client
            .post(format!("{}/chat/completions", self.base_url.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let raw = resp.text().await?;
        tracing::debug!("LLM raw response ({}): {}", status, &raw[..raw.len().min(500)]);
        let data: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("JSON parse error: {} | body: {}", e, &raw[..raw.len().min(300)]))?;
        if !status.is_success() {
            return Err(anyhow::anyhow!("LLM API error {}: {}", status, &raw[..raw.len().min(300)]));
        }
        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("No content in LLM response"))?
            .to_string();

        Ok(strip_code_fences(&content))
    }
}

fn strip_code_fences(s: &str) -> String {
    let s = s.trim();
    let s = if s.starts_with("```json") {
        &s[7..]
    } else if s.starts_with("```") {
        &s[3..]
    } else {
        s
    };
    let s = if s.ends_with("```") {
        &s[..s.len() - 3]
    } else {
        s
    };
    s.trim().to_string()
}
