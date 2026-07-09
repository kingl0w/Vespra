use std::sync::Arc;

use anyhow::Result;

use crate::agents::AgentClient;

const SYSTEM_PROMPT: &str = "\
You are Vespra's conversational interface. You help users understand what Vespra is doing, \
what it can do, and answer questions about its current state and capabilities.

RULES:
- Respond in plain English prose only. Never output JSON, code blocks, structured data, or markdown formatting.
- Keep answers concise: 1-3 sentences for simple questions, a short paragraph for complex ones.
- You will receive live context about active goals, sentinel status, and system health. Use it to answer accurately.
- If you don't know something or the context doesn't contain the answer, say so honestly rather than guessing.
- Never echo the user's question back. Never respond with just a question.
- You are helpful, direct, and confident.

ABOUT VESPRA:
Vespra is an autonomous DeFi trading system. It runs goal-based strategies (Compound, YieldRotate, Snipe, Adaptive) \
on Base/Arbitrum chains. Each goal goes through a pipeline: Scouting → Risk Assessment → Trading → Execution → \
Monitoring → Exiting → Compounding. A Sentinel agent monitors positions every 5 minutes for stop-loss/target triggers. \
A Yield Scheduler checks for better APY opportunities every 30 minutes. Goals auto-resume on gateway restart.";

pub struct ChatHandler {
    llm: Arc<dyn AgentClient>,
}

impl ChatHandler {
    pub fn new(llm: Arc<dyn AgentClient>) -> Self {
        Self { llm }
    }

    pub async fn respond(&self, user_message: &str, live_context: &str) -> Result<String> {
        let task = if live_context.is_empty() {
            user_message.to_string()
        } else {
            format!(
                "[LIVE SYSTEM STATE]\n{live_context}\n\n[USER MESSAGE]\n{user_message}"
            )
        };

        let raw = self.llm.call(SYSTEM_PROMPT, &task).await?;
        Ok(sanitize_llm_prose(&raw))
    }
}

///ves-93b: the system prompt tells the LLM to return prose only, but some
///models still wrap their answer as `{"message": "..."}`, return an array
///of strings, etc. strip the wrapping before it reaches the dashboard so
///users don't see raw JSON in the chat transcript.
pub fn sanitize_llm_prose(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    //fast path: if it doesn't look like JSON, don't bother parsing.
    let looks_like_json = trimmed.starts_with('{') || trimmed.starts_with('[');
    if !looks_like_json {
        return trimmed.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => unwrap_value(&v),
        //valid-looking but malformed JSON — hand back the raw text, not an
        //empty string; the user still benefits from seeing what came back.
        Err(_) => trimmed.to_string(),
    }
}

///prose keys we'll unwrap, in priority order. `message` wins over `response`
///wins over `text` etc. so that an object carrying several fields still
///picks the one most likely to be the user-facing prose.
const PROSE_KEYS: &[&str] = &["message", "response", "text", "content", "result", "output"];

fn unwrap_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => v.to_string(),
        serde_json::Value::Array(items) => items
            .iter()
            .map(unwrap_value)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        serde_json::Value::Object(map) => {
            for key in PROSE_KEYS {
                if let Some(inner) = map.get(*key) {
                    return unwrap_value(inner);
                }
            }
            //unknown shape — pretty-print so it's at least legible, not a
            //one-line blob. keep the raw json so developers can still
            //debug from the transcript.
            serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_passes_through() {
        assert_eq!(sanitize_llm_prose("Hello, world."), "Hello, world.");
        assert_eq!(sanitize_llm_prose("  trimmed  "), "trimmed");
    }

    #[test]
    fn unwraps_message_key() {
        assert_eq!(
            sanitize_llm_prose(r#"{"message": "All systems green."}"#),
            "All systems green."
        );
    }

    #[test]
    fn unwraps_each_prose_key() {
        for key in ["message", "response", "text", "content", "result", "output"] {
            let raw = format!(r#"{{"{key}": "hi"}}"#);
            assert_eq!(sanitize_llm_prose(&raw), "hi", "failed for key {key}");
        }
    }

    #[test]
    fn message_wins_over_response() {
        //priority: message > response > text ...
        let raw = r#"{"response": "b", "message": "a", "text": "c"}"#;
        assert_eq!(sanitize_llm_prose(raw), "a");
    }

    #[test]
    fn array_joined_as_paragraphs() {
        let raw = r#"["first line", "second line"]"#;
        assert_eq!(sanitize_llm_prose(raw), "first line\n\nsecond line");
    }

    #[test]
    fn array_of_wrapped_objects() {
        let raw = r#"[{"message": "one"}, {"message": "two"}]"#;
        assert_eq!(sanitize_llm_prose(raw), "one\n\ntwo");
    }

    #[test]
    fn unknown_object_shape_pretty_prints_instead_of_object_object() {
        let raw = r#"{"foo": 1, "bar": 2}"#;
        let out = sanitize_llm_prose(raw);
        //not the raw one-liner, not empty, not "[object Object]"
        assert!(out.contains("\"foo\""));
        assert!(out.contains('\n'));
        assert!(!out.contains("[object Object]"));
    }

    #[test]
    fn nested_wrapper_is_unwrapped() {
        let raw = r#"{"response": {"message": "deep"}}"#;
        assert_eq!(sanitize_llm_prose(raw), "deep");
    }

    #[test]
    fn malformed_json_returns_raw_text() {
        let raw = r#"{"message": "unterminated"#;
        assert_eq!(sanitize_llm_prose(raw), raw);
    }

    #[test]
    fn empty_input_is_empty_output() {
        assert_eq!(sanitize_llm_prose(""), "");
        assert_eq!(sanitize_llm_prose("   "), "");
    }

    #[test]
    fn prose_containing_braces_in_middle_is_preserved() {
        //only the fast path triggers on leading '{' — prose that happens
        //to mention JSON in passing is untouched.
        let prose = "Use curly braces like {key: value} to denote maps.";
        assert_eq!(sanitize_llm_prose(prose), prose);
    }
}
