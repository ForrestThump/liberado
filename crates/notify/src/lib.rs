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

    /// Notify about a proposal awaiting approval, offering action buttons/replies on channels
    /// that support them. `proposal_id` is the proposal's filename stem (see
    /// `liberado_common::Proposal`'s note-writing convention) — the correlation id a tap on this
    /// channel needs to act back on the right proposal. Defaults to plain [`notify`](Self::notify)
    /// so only channels that actually support interactive replies (Telegram) need to override this.
    async fn notify_proposal(&self, proposal_id: &str, message: &str) -> Result<(), NotifyError> {
        let _ = proposal_id;
        self.notify(message).await
    }

    /// Deliver a scheduled (cron) session's finished result. Distinct from [`notify`](Self::notify)
    /// because a channel may choose to *fold this into the ongoing conversation* and/or *defer it
    /// around the human's activity* — the motivating case is the server's chat-delivering notifier,
    /// which appends the brief to the sticky Telegram chat session (so a reply has it in context) and
    /// holds it until you're between messages. Defaults to a plain, immediate [`notify`](Self::notify)
    /// so every other channel (and tests) behave exactly as before.
    async fn deliver_cron(&self, message: &str) -> Result<(), NotifyError> {
        self.notify(message).await
    }
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

    /// Build from `LIBERADO_TELEGRAM_BOT_TOKEN` + `LIBERADO_TELEGRAM_CHAT_ID`, or `None` if
    /// either is unset — notifications are opt-in, not required for the daemon to run at all.
    ///
    /// Names are Liberado-prefixed on purpose: OpenClaw (and other bots on the same host) already
    /// own `TELEGRAM_BOT_TOKEN` / `TELEGRAM_CHAT_ID`. Sharing those names would collide.
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("LIBERADO_TELEGRAM_BOT_TOKEN").ok()?;
        let chat_id = std::env::var("LIBERADO_TELEGRAM_CHAT_ID").ok()?;
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

/// Telegram limits `callback_data` to 64 bytes. The longest action prefix used on this button row
/// is `"approve:"` (8 bytes); capping the id itself well under the remainder leaves headroom for
/// longer action names later without ever risking a callback Telegram would reject outright.
const MAX_CALLBACK_PROPOSAL_ID_LEN: usize = 50;

/// Pure — whether `proposal_id` is safe to embed in Telegram `callback_data`. Separated from
/// `notify_proposal` so the boundary condition is unit-testable without a network call.
fn fits_in_callback_data(proposal_id: &str) -> bool {
    proposal_id.len() <= MAX_CALLBACK_PROPOSAL_ID_LEN
}

/// Pure — the `sendMessage` payload for an Approve/Revise/Reject proposal notification. Separated
/// from `notify_proposal` so the button/callback_data shape is unit-testable without a network
/// call. The Revise tap is answered by `liberado-telegram-approvals`' `ApprovalBot`, which prompts
/// for free text and hands it to the shared provider — never this crate, which stays a one-way
/// send.
fn approval_buttons_payload(chat_id: &str, message: &str, proposal_id: &str) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "text": message,
        "reply_markup": {
            "inline_keyboard": [[
                { "text": "✅ Approve", "callback_data": format!("approve:{proposal_id}") },
                { "text": "📝 Revise", "callback_data": format!("revise:{proposal_id}") },
                { "text": "❌ Reject", "callback_data": format!("reject:{proposal_id}") }
            ]]
        }
    })
}

impl TelegramNotifier {
    /// POST `payload` to `sendMessage` and translate a non-2xx response into a [`NotifyError`] —
    /// the shared body behind both [`Notifier::notify`] and [`Notifier::notify_proposal`].
    async fn send(&self, payload: serde_json::Value) -> Result<(), NotifyError> {
        let response = self
            .client
            .post(self.send_message_url())
            .json(&payload)
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

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify(&self, message: &str) -> Result<(), NotifyError> {
        self.send(serde_json::json!({ "chat_id": self.chat_id, "text": message }))
            .await
    }

    /// Sends the message with Approve/Reject inline-keyboard buttons whose `callback_data` is
    /// `"approve:{proposal_id}"` / `"reject:{proposal_id}"` — a proposal-approval bot (see
    /// `liberado-telegram-approvals`) correlates a tap back to `proposals/{proposal_id}.md`.
    /// Falls back to a plain [`notify`](Self::notify) call when `proposal_id` is too long for
    /// Telegram's `callback_data` budget, rather than sending a callback the API would reject.
    async fn notify_proposal(&self, proposal_id: &str, message: &str) -> Result<(), NotifyError> {
        if !fits_in_callback_data(proposal_id) {
            tracing::warn!(
                proposal_id,
                len = proposal_id.len(),
                "proposal id too long for Telegram callback_data — sending a plain notification instead"
            );
            return self.notify(message).await;
        }

        self.send(approval_buttons_payload(
            &self.chat_id,
            message,
            proposal_id,
        ))
        .await
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

    #[test]
    fn a_short_proposal_id_fits_in_callback_data() {
        assert!(fits_in_callback_data("prop-sub-1"));
        assert!(fits_in_callback_data(
            &"a".repeat(MAX_CALLBACK_PROPOSAL_ID_LEN)
        ));
    }

    #[test]
    fn an_overlong_proposal_id_does_not_fit_in_callback_data() {
        assert!(!fits_in_callback_data(
            &"a".repeat(MAX_CALLBACK_PROPOSAL_ID_LEN + 1)
        ));
    }

    #[test]
    fn approval_buttons_payload_embeds_prefixed_callback_data() {
        let payload = approval_buttons_payload("42", "a proposal needs review", "prop-1");
        assert_eq!(payload["chat_id"], "42");
        assert_eq!(payload["text"], "a proposal needs review");
        let buttons = &payload["reply_markup"]["inline_keyboard"][0];
        assert_eq!(buttons[0]["callback_data"], "approve:prop-1");
        assert_eq!(buttons[1]["callback_data"], "revise:prop-1");
        assert_eq!(buttons[2]["callback_data"], "reject:prop-1");
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
    #[ignore = "requires LIBERADO_TELEGRAM_BOT_TOKEN + LIBERADO_TELEGRAM_CHAT_ID + network access"]
    async fn live_send_via_from_env_actually_delivers() {
        let notifier = TelegramNotifier::from_env()
            .expect("set LIBERADO_TELEGRAM_BOT_TOKEN and LIBERADO_TELEGRAM_CHAT_ID to run this test");
        notifier
            .notify("liberado-notify: live test via cargo test -- --ignored")
            .await
            .expect("a real send with real credentials must succeed");
    }
}
