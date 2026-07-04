//! # liberado-notify
//!
//! A minimal, pluggable notification sink for events a human should know about even when nothing
//! else in the daemon's own surfaces (TUI, WebUI, an open chat) is in front of them right now —
//! the motivating case is a cron-triggered (Phase 3) proposal nobody happens to be watching for.
//! Deliberately a trait, not a hardcoded channel: Telegram is the first implementation (free,
//! mature, works today); a future push-notification channel is a new impl, not a rewrite.
//!
//! Notifications are always best-effort. A failed notification must never abort or block the
//! caller's real work — the proposal file still gets written even if telling a human about it
//! fails; callers log the failure and move on, they never propagate it as their own error.

use async_trait::async_trait;

/// Something that can be told about an event worth a human's attention.
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, message: &str) -> Result<(), NotifyError>;
}

/// A notification failed to send. Never a reason to abort the caller's own work — see the module
/// doc comment.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NotifyError(pub String);

/// Sends a message to one Telegram chat via the Bot API's `sendMessage` method
/// (<https://core.telegram.org/bots/api#sendmessage>).
pub struct TelegramNotifier {
    client: reqwest::Client,
    token: String,
    chat_id: String,
}

impl TelegramNotifier {
    pub fn new(token: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.into(),
            chat_id: chat_id.into(),
        }
    }

    /// Build from `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`, or `None` if either is unset —
    /// notifications are opt-in, not required for the daemon to run at all.
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        let chat_id = std::env::var("TELEGRAM_CHAT_ID").ok()?;
        Some(Self::new(token, chat_id))
    }

    fn send_message_url(&self) -> String {
        send_message_url(&self.token)
    }
}

/// Pure, independent of any network call — the part worth unit-testing directly rather than
/// through a live (or mocked) HTTP round trip.
fn send_message_url(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}/sendMessage")
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify(&self, message: &str) -> Result<(), NotifyError> {
        let response = self
            .client
            .post(self.send_message_url())
            .json(&serde_json::json!({ "chat_id": self.chat_id, "text": message }))
            .send()
            .await
            .map_err(|e| NotifyError(format!("Telegram request failed: {e}")))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(NotifyError(format!("Telegram API error: {body}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_message_url_embeds_the_token() {
        assert_eq!(
            send_message_url("abc123"),
            "https://api.telegram.org/botabc123/sendMessage"
        );
    }

    #[tokio::test]
    #[ignore = "hits the real Telegram API — requires network access"]
    async fn notify_against_an_invalid_token_is_a_real_error_not_a_panic() {
        // No mocking here (per this project's testing philosophy — live testing is the accepted
        // complement to unit tests, not something to chase via complex mocks); this just proves a
        // bad token/network failure surfaces as `Err`, not a panic or a silent `Ok`. Ignored by
        // default since it's a real network call, same convention as this workspace's other
        // live/network tests.
        let notifier = TelegramNotifier::new("not-a-real-token", "0");
        let result = notifier.notify("test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID + network access"]
    async fn live_send_via_from_env_actually_delivers() {
        let notifier = TelegramNotifier::from_env()
            .expect("set TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID to run this test");
        notifier
            .notify("liberado-notify: live test via cargo test -- --ignored")
            .await
            .expect("a real send with real credentials must succeed");
    }
}
