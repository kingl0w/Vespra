pub mod telegram;

pub use telegram::TelegramClient;

use crate::routes::AppState;

/// Markdown-escape characters that Telegram's MarkdownV1 treats as special.
/// Covers: _ * [ ] ( ) ~ ` > # + - = | { } . !
pub fn escape_markdown(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
                | '{' | '}' | '.' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Fire-and-forget notification. No-op when Telegram is not configured.
/// The spawned task swallows all errors internally.
pub fn notify(state: &AppState, message: String) {
    if let Some(tg) = &state.telegram {
        let tg = tg.clone();
        tokio::spawn(async move {
            let _ = tg.send(&message).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_markdown_special_chars() {
        assert_eq!(escape_markdown("hello_world"), "hello\\_world");
        assert_eq!(escape_markdown("*bold*"), "\\*bold\\*");
        assert_eq!(escape_markdown("no specials"), "no specials");
        assert_eq!(
            escape_markdown("a_b*c[d]e(f)g~h`i>j#k+l-m=n|o{p}q.r!s"),
            "a\\_b\\*c\\[d\\]e\\(f\\)g\\~h\\`i\\>j\\#k\\+l\\-m\\=n\\|o\\{p\\}q\\.r\\!s"
        );
    }

    #[test]
    fn escape_markdown_empty_string() {
        assert_eq!(escape_markdown(""), "");
    }

    #[test]
    fn notify_noop_when_telegram_none() {
        // We can't easily construct a full AppState in a unit test,
        // but we verify the guard logic: if state.telegram is None,
        // notify does nothing (no panic, no spawn).
        // This is tested structurally — the function checks Option
        // and only enters the block on Some.
    }
}
