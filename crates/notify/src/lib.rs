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
//!
//! ## Messaging channels
//!
//! Telegram also implements [`liberado_messaging::MessagingChannel`] (duplex transport used by the
//! approval/chat bot). Future clients (Matrix, Signal, Discord) implement that trait; wrap any
//! channel as a [`Notifier`] with [`ChannelNotifier`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use liberado_messaging::{
    ActionButton, InboundEvent, MessagingChannel, MessagingError, approval_action_rows,
    permission_action_rows,
};

/// Something that can be told about an event worth a human's attention.
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, message: &str) -> Result<(), NotifyError>;

    /// Notify about a proposal awaiting approval, offering action buttons/replies on channels
    /// that support them. `proposal_id` is the proposal's filename stem (see
    /// `liberado_common::Proposal`'s note-writing convention) — the correlation id a tap on this
    /// channel needs to act back on the right proposal. Defaults to plain [`notify`](Self::notify)
    /// so only channels that actually support interactive replies need to override this.
    async fn notify_proposal(&self, proposal_id: &str, message: &str) -> Result<(), NotifyError> {
        let _ = proposal_id;
        self.notify(message).await
    }

    /// Notify about a **permission request** — an agent hit a zone it wasn't granted and is asking
    /// the human to expand its authority. Offers four scope buttons (Deny / Approve once / Approve
    /// this session / Approve everywhere); a tap is handled by the messaging approval bot, which
    /// records the choice onto `proposals/{proposal_id}.md`. Defaults to plain [`notify`](Self::notify)
    /// so non-interactive channels degrade gracefully.
    async fn notify_permission_request(
        &self,
        proposal_id: &str,
        message: &str,
    ) -> Result<(), NotifyError> {
        let _ = proposal_id;
        self.notify(message).await
    }

    /// Deliver a scheduled (cron) session's finished result. Distinct from [`notify`](Self::notify)
    /// because a channel may choose to *fold this into the ongoing conversation* and/or *defer it
    /// around the human's activity* — the motivating case is the server's chat-delivering notifier,
    /// which appends the brief to the sticky chat session (so a reply has it in context) and
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

impl From<MessagingError> for NotifyError {
    fn from(e: MessagingError) -> Self {
        NotifyError(e.0)
    }
}

/// Wraps any [`MessagingChannel`] as a one-way [`Notifier`].
///
/// Use this when a future Matrix/Discord/Signal channel should also drive unattended proposal
/// pings — one transport, both seams.
pub struct ChannelNotifier {
    channel: Arc<dyn MessagingChannel>,
}

impl ChannelNotifier {
    pub fn new(channel: Arc<dyn MessagingChannel>) -> Self {
        Self { channel }
    }

    pub fn channel(&self) -> Arc<dyn MessagingChannel> {
        self.channel.clone()
    }
}

#[async_trait]
impl Notifier for ChannelNotifier {
    async fn notify(&self, message: &str) -> Result<(), NotifyError> {
        self.channel.send_text(message).await.map_err(Into::into)
    }

    async fn notify_proposal(&self, proposal_id: &str, message: &str) -> Result<(), NotifyError> {
        self.channel
            .send_with_actions(message, &approval_action_rows(proposal_id))
            .await
            .map_err(Into::into)
    }

    async fn notify_permission_request(
        &self,
        proposal_id: &str,
        message: &str,
    ) -> Result<(), NotifyError> {
        self.channel
            .send_with_actions(message, &permission_action_rows(proposal_id))
            .await
            .map_err(Into::into)
    }
}

// ── Telegram transport ────────────────────────────────────────────────────────

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";
/// Telegram hard-caps message text at 4096 UTF-16 code units; stay under with headroom.
const TELEGRAM_MAX_MESSAGE_CHARS: usize = 4000;
/// Telegram limits `callback_data` to 64 bytes. Longest action prefix we use is `"everywhere:"`
/// (11 bytes); capping the correlation id itself well under the remainder leaves headroom.
const MAX_CALLBACK_CORRELATION_ID_LEN: usize = 50;

/// Telegram Bot API transport — implements both [`Notifier`] (one-way) and
/// [`MessagingChannel`] (duplex, used by the approval/chat bot).
///
/// Env: `LIBERADO_TELEGRAM_BOT_TOKEN` + `LIBERADO_TELEGRAM_CHAT_ID` (Liberado-prefixed so we do not
/// collide with OpenClaw's unprefixed names on a shared host).
#[derive(Clone)]
pub struct TelegramNotifier {
    client: reqwest::Client,
    token: String,
    chat_id: String,
    /// Full API base (including the `/bot` prefix) — the real Telegram host by default, or a
    /// test mock server's URL. Injected as a field so transport tests can point at a wiremock
    /// server instead of hitting the network (same shape as `sessions_root` / `run_sync_in`).
    api_base: String,
    /// Long-poll timeout for `getUpdates` (seconds). Telegram caps this at 50.
    getupdate_timeout_secs: u64,
    /// Sleep after a failed `getUpdates` before retrying.
    poll_retry_backoff_secs: u64,
}

/// Prefer this name when treating Telegram as a [`MessagingChannel`]. Same type as
/// [`TelegramNotifier`] — kept as an alias so composition roots can speak either vocabulary.
pub type TelegramChannel = TelegramNotifier;

impl TelegramNotifier {
    pub fn new(token: impl Into<String>, chat_id: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            token: token.into(),
            chat_id: chat_id.into(),
            api_base: TELEGRAM_API_BASE.to_string(),
            getupdate_timeout_secs: 25,
            poll_retry_backoff_secs: 10,
        }
    }

    /// Override the API base URL — used only by tests to point at a local mock server.
    #[allow(dead_code)] // exercised by #[cfg(test)]; harmless to expose for composition.
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    /// Override long-poll timing (from `config.tuning.telegram_approvals`).
    pub fn with_poll_tuning(
        mut self,
        getupdate_timeout_secs: u64,
        poll_retry_backoff_secs: u64,
    ) -> Self {
        self.getupdate_timeout_secs = getupdate_timeout_secs;
        self.poll_retry_backoff_secs = poll_retry_backoff_secs;
        self
    }

    /// Build from `LIBERADO_TELEGRAM_BOT_TOKEN` + `LIBERADO_TELEGRAM_CHAT_ID`, or `None` if
    /// either is unset — notifications are opt-in, not required for the daemon to run at all.
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("LIBERADO_TELEGRAM_BOT_TOKEN").ok()?;
        let chat_id = std::env::var("LIBERADO_TELEGRAM_CHAT_ID").ok()?;
        Some(Self::new(token, chat_id))
    }

    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", self.api_base, self.token, method)
    }

    /// POST `payload` to `sendMessage` and translate a non-2xx response into a [`MessagingError`].
    async fn send_message_payload(&self, payload: serde_json::Value) -> Result<(), MessagingError> {
        let response = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&payload)
            .send()
            .await
            .map_err(|e| MessagingError(format!("Telegram request failed: {e}")))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MessagingError(format!("Telegram API error: {body}")));
        }
        Ok(())
    }

    fn message_is_from_allowed_chat(&self, msg: &serde_json::Value) -> bool {
        let Some(id) = msg.get("chat").and_then(|c| c.get("id")) else {
            return false;
        };
        // Telegram may send chat.id as a number; env stores a string.
        let incoming = id
            .as_i64()
            .map(|n| n.to_string())
            .or_else(|| id.as_str().map(str::to_string))
            .unwrap_or_default();
        incoming == self.chat_id
    }

    fn encode_callback_data(action: &str, correlation_id: &str) -> String {
        format!("{action}:{correlation_id}")
    }

    fn parse_callback_data(data: &str) -> Option<(&str, &str)> {
        data.split_once(':')
    }

    fn actions_to_inline_keyboard(rows: &[Vec<ActionButton>]) -> serde_json::Value {
        let keyboard: Vec<Vec<serde_json::Value>> = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|b| {
                        serde_json::json!({
                            "text": b.label,
                            "callback_data": Self::encode_callback_data(&b.action, &b.correlation_id),
                        })
                    })
                    .collect()
            })
            .collect();
        serde_json::json!({ "inline_keyboard": keyboard })
    }

    /// Whether every correlation id in `rows` fits Telegram's `callback_data` budget.
    fn actions_fit_callback_budget(rows: &[Vec<ActionButton>]) -> bool {
        rows.iter().flatten().all(|b| {
            b.correlation_id.len() <= MAX_CALLBACK_CORRELATION_ID_LEN
                && Self::encode_callback_data(&b.action, &b.correlation_id).len() <= 64
        })
    }
}

/// Split on char boundaries into chunks that fit Telegram's message size limit.
fn split_telegram_chunks(text: &str) -> Vec<String> {
    if text.chars().count() <= TELEGRAM_MAX_MESSAGE_CHARS {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0usize;
    for ch in text.chars() {
        if count >= TELEGRAM_MAX_MESSAGE_CHARS {
            chunks.push(std::mem::take(&mut current));
            count = 0;
        }
        current.push(ch);
        count += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[async_trait]
impl MessagingChannel for TelegramNotifier {
    fn name(&self) -> &str {
        "Telegram"
    }

    async fn send_text(&self, text: &str) -> Result<(), MessagingError> {
        for chunk in split_telegram_chunks(text) {
            self.send_message_payload(serde_json::json!({
                "chat_id": self.chat_id,
                "text": chunk,
            }))
            .await?;
        }
        Ok(())
    }

    async fn send_with_actions(
        &self,
        text: &str,
        rows: &[Vec<ActionButton>],
    ) -> Result<(), MessagingError> {
        if !Self::actions_fit_callback_budget(rows) {
            tracing::warn!(
                "action correlation id too long for Telegram callback_data — sending plain text"
            );
            return self.send_text(text).await;
        }
        // Buttons only on the first chunk; subsequent chunks (if any) are plain follow-ups.
        let mut chunks = split_telegram_chunks(text).into_iter();
        let Some(first) = chunks.next() else {
            return Ok(());
        };
        self.send_message_payload(serde_json::json!({
            "chat_id": self.chat_id,
            "text": first,
            "reply_markup": Self::actions_to_inline_keyboard(rows),
        }))
        .await?;
        for chunk in chunks {
            self.send_message_payload(serde_json::json!({
                "chat_id": self.chat_id,
                "text": chunk,
            }))
            .await?;
        }
        Ok(())
    }

    async fn request_reply(&self, prompt: &str) -> Result<String, MessagingError> {
        let response = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": prompt,
                "reply_markup": {
                    "force_reply": true,
                    "input_field_placeholder": "Describe the changes needed..."
                }
            }))
            .send()
            .await
            .map_err(|e| MessagingError(format!("Telegram request_reply failed: {e}")))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MessagingError(format!("Telegram API error: {body}")));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| MessagingError(format!("Telegram request_reply decode: {e}")))?;
        let message_id = body
            .get("result")
            .and_then(|r| r.get("message_id"))
            .and_then(|id| id.as_i64())
            .ok_or_else(|| MessagingError("Telegram request_reply: missing message_id".into()))?;
        Ok(message_id.to_string())
    }

    async fn acknowledge(&self, event_id: &str, text: &str) -> Result<(), MessagingError> {
        let response = self
            .client
            .post(self.api_url("answerCallbackQuery"))
            .json(&serde_json::json!({
                "callback_query_id": event_id,
                "text": text,
            }))
            .send()
            .await
            .map_err(|e| MessagingError(format!("Telegram acknowledge failed: {e}")))?;
        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MessagingError(format!("Telegram API error: {body}")));
        }
        Ok(())
    }

    async fn edit_message(&self, message_ref: &str, text: &str) -> Result<(), MessagingError> {
        let message_id: i64 = message_ref
            .parse()
            .map_err(|_| MessagingError(format!("edit_message: bad message id {message_ref:?}")))?;
        // Omitting `reply_markup` on editMessageText clears the inline keyboard — that's the
        // button-stripping half; the new `text` is the receipt.
        let response = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "message_id": message_id,
                "text": text,
            }))
            .send()
            .await
            .map_err(|e| MessagingError(format!("Telegram editMessageText failed: {e}")))?;
        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(MessagingError(format!("Telegram API error: {body}")));
        }
        Ok(())
    }

    async fn set_typing(&self) -> Result<(), MessagingError> {
        let _ = self
            .client
            .post(self.api_url("sendChatAction"))
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "action": "typing",
            }))
            .send()
            .await;
        Ok(())
    }

    async fn register_commands(&self, commands: &[(String, String)]) -> Result<(), MessagingError> {
        if commands.is_empty() {
            return Ok(());
        }
        let commands: Vec<serde_json::Value> = commands
            .iter()
            .map(|(command, description)| {
                serde_json::json!({ "command": command, "description": description })
            })
            .collect();
        let body = serde_json::json!({
            "commands": commands,
            // Scope to this chat so the menu appears for the operator without publishing it globally.
            "scope": { "type": "chat", "chat_id": self.chat_id },
        });
        let response = self
            .client
            .post(self.api_url("setMyCommands"))
            .json(&body)
            .send()
            .await
            .map_err(|e| MessagingError(format!("Telegram setMyCommands failed: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(MessagingError(format!(
                "setMyCommands non-success: {status}"
            )));
        }
        Ok(())
    }

    async fn receive(&self, cursor: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
        let offset: i64 = cursor.parse().unwrap_or(0);
        let timeout = self.getupdate_timeout_secs;
        let url = format!(
            "{}?offset={offset}&timeout={timeout}&allowed_updates=[\"message\",\"callback_query\"]",
            self.api_url("getUpdates"),
        );
        let response = match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(timeout + 5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                tracing::warn!(status = %r.status(), "Telegram getUpdates non-success");
                tokio::time::sleep(Duration::from_secs(self.poll_retry_backoff_secs)).await;
                return Ok(vec![]);
            }
            Err(e) => {
                tracing::warn!("Telegram getUpdates error: {e}");
                tokio::time::sleep(Duration::from_secs(self.poll_retry_backoff_secs)).await;
                return Ok(vec![]);
            }
        };

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| MessagingError(format!("Telegram getUpdates decode: {e}")))?;
        let updates = body["result"].as_array().cloned().unwrap_or_default();

        let mut events = Vec::new();
        for update in updates {
            let update_id = update["update_id"].as_i64().unwrap_or(0);
            *cursor = (update_id + 1).to_string();

            if let Some(cq) = update.get("callback_query") {
                let event_id = cq["id"].as_str().unwrap_or("").to_string();
                let data = cq["data"].as_str().unwrap_or("");
                // The message the button lives on — its id lets us edit it (strip buttons + stamp a
                // receipt) once the tap is handled.
                let message_ref = cq
                    .get("message")
                    .and_then(|m| m.get("message_id"))
                    .and_then(|id| id.as_i64())
                    .map(|id| id.to_string());
                if let Some((action, correlation_id)) = Self::parse_callback_data(data) {
                    events.push(InboundEvent::Action {
                        action: action.to_string(),
                        correlation_id: correlation_id.to_string(),
                        event_id,
                        message_ref,
                    });
                } else {
                    tracing::warn!(data, "unexpected Telegram callback_data format");
                }
                continue;
            }

            if let Some(msg) = update.get("message") {
                if !self.message_is_from_allowed_chat(msg) {
                    tracing::debug!(
                        chat = ?msg.get("chat").and_then(|c| c.get("id")),
                        "Telegram message from non-configured chat — ignored"
                    );
                    continue;
                }
                let from_bot = msg
                    .get("from")
                    .and_then(|f| f.get("is_bot"))
                    .and_then(|b| b.as_bool())
                    == Some(true);
                let text = msg["text"].as_str().unwrap_or("").to_string();
                let reply_to_prompt = msg
                    .get("reply_to_message")
                    .and_then(|r| r["message_id"].as_i64())
                    .map(|id| id.to_string());
                events.push(InboundEvent::Message {
                    text,
                    reply_to_prompt,
                    from_bot,
                });
            }
        }
        Ok(events)
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn notify(&self, message: &str) -> Result<(), NotifyError> {
        self.send_text(message).await.map_err(Into::into)
    }

    async fn notify_proposal(&self, proposal_id: &str, message: &str) -> Result<(), NotifyError> {
        self.send_with_actions(message, &approval_action_rows(proposal_id))
            .await
            .map_err(Into::into)
    }

    async fn notify_permission_request(
        &self,
        proposal_id: &str,
        message: &str,
    ) -> Result<(), NotifyError> {
        self.send_with_actions(message, &permission_action_rows(proposal_id))
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_url_embeds_the_token_and_method() {
        let n = TelegramNotifier::new("abc123", "42");
        assert_eq!(
            n.api_url("sendMessage"),
            "https://api.telegram.org/botabc123/sendMessage"
        );
    }

    #[test]
    fn split_telegram_chunks_keeps_short_messages_whole() {
        assert_eq!(split_telegram_chunks("hi"), vec!["hi".to_string()]);
    }

    #[test]
    fn split_telegram_chunks_splits_long_messages() {
        let long: String = "a".repeat(TELEGRAM_MAX_MESSAGE_CHARS + 50);
        let chunks = split_telegram_chunks(&long);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), TELEGRAM_MAX_MESSAGE_CHARS);
        assert_eq!(chunks[1].chars().count(), 50);
    }

    #[test]
    fn callback_data_round_trips_action_and_correlation() {
        let encoded = TelegramNotifier::encode_callback_data("approve", "prop-1");
        assert_eq!(
            TelegramNotifier::parse_callback_data(&encoded),
            Some(("approve", "prop-1"))
        );
        // split_once: robust even if a stem ever contained a colon.
        assert_eq!(
            TelegramNotifier::parse_callback_data("reject:vault-change-inbox-x-md-abc"),
            Some(("reject", "vault-change-inbox-x-md-abc"))
        );
        assert_eq!(TelegramNotifier::parse_callback_data("no-colon"), None);
    }

    #[test]
    fn actions_fit_budget_for_max_length_id() {
        let max_id = "x".repeat(MAX_CALLBACK_CORRELATION_ID_LEN);
        let rows = permission_action_rows(&max_id);
        assert!(TelegramNotifier::actions_fit_callback_budget(&rows));
        let overlong = "x".repeat(MAX_CALLBACK_CORRELATION_ID_LEN + 1);
        assert!(!TelegramNotifier::actions_fit_callback_budget(
            &approval_action_rows(&overlong)
        ));
    }

    #[test]
    fn inline_keyboard_embeds_callback_data() {
        let rows = approval_action_rows("prop-1");
        let markup = TelegramNotifier::actions_to_inline_keyboard(&rows);
        let buttons = &markup["inline_keyboard"][0];
        assert_eq!(buttons[0]["callback_data"], "approve:prop-1");
        assert_eq!(buttons[1]["callback_data"], "revise:prop-1");
        assert_eq!(buttons[2]["callback_data"], "reject:prop-1");
    }

    #[tokio::test]
    #[ignore = "hits the real Telegram API — requires network access"]
    async fn notify_against_an_invalid_token_is_a_real_error_not_a_panic() {
        let notifier = TelegramNotifier::new("not-a-real-token", "0");
        let result = notifier.notify("test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires LIBERADO_TELEGRAM_BOT_TOKEN + LIBERADO_TELEGRAM_CHAT_ID + network access"]
    async fn live_send_via_from_env_actually_delivers() {
        let notifier = TelegramNotifier::from_env().expect(
            "set LIBERADO_TELEGRAM_BOT_TOKEN and LIBERADO_TELEGRAM_CHAT_ID to run this test",
        );
        notifier
            .notify("liberado-notify: live test via cargo test -- --ignored")
            .await
            .expect("a real send with real credentials must succeed");
    }

    /// A notifier that only implements `notify` — records what it was told, so the default
    /// `notify_proposal` / `notify_permission_request` / `deliver_cron` impls can be asserted to
    /// actually delegate to `notify` rather than just return `Ok`.
    struct NotifyOnlyNotifier {
        told: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl NotifyOnlyNotifier {
        fn new() -> Self {
            Self {
                told: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl Notifier for NotifyOnlyNotifier {
        async fn notify(&self, message: &str) -> Result<(), NotifyError> {
            self.told.lock().unwrap().push(message.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn default_notify_proposal_delegates_to_notify() {
        let n = NotifyOnlyNotifier::new();
        let told = n.told.clone();
        assert!(
            n.notify_proposal("prop-1", "approve proposal x")
                .await
                .is_ok()
        );
        assert_eq!(told.lock().unwrap().as_slice(), &["approve proposal x"]);
    }

    #[tokio::test]
    async fn default_notify_permission_request_delegates_to_notify() {
        let n = NotifyOnlyNotifier::new();
        let told = n.told.clone();
        assert!(
            n.notify_permission_request("perm-1", "request access to Work zone")
                .await
                .is_ok()
        );
        assert_eq!(
            told.lock().unwrap().as_slice(),
            &["request access to Work zone"]
        );
    }

    #[tokio::test]
    async fn default_deliver_cron_delegates_to_notify() {
        let n = NotifyOnlyNotifier::new();
        let told = n.told.clone();
        assert!(n.deliver_cron("cron message").await.is_ok());
        assert_eq!(told.lock().unwrap().as_slice(), &["cron message"]);
    }

    /// A recording [`MessagingChannel`] for testing [`ChannelNotifier`].
    struct RecordingChannel {
        sent: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl liberado_messaging::MessagingChannel for RecordingChannel {
        fn name(&self) -> &str {
            "test-recording"
        }
        async fn send_text(&self, msg: &str) -> Result<(), MessagingError> {
            self.sent.lock().unwrap().push(msg.to_string());
            Ok(())
        }
        async fn send_with_actions(
            &self,
            msg: &str,
            _rows: &[Vec<ActionButton>],
        ) -> Result<(), MessagingError> {
            self.sent.lock().unwrap().push(msg.to_string());
            Ok(())
        }
        async fn request_reply(&self, _prompt: &str) -> Result<String, MessagingError> {
            Ok("reply-id".into())
        }
        async fn acknowledge(&self, _event_id: &str, _text: &str) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn receive(&self, _cursor: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn channel_notifier_sends_via_inner_channel() {
        let channel = Arc::new(RecordingChannel {
            sent: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let sent = channel.sent.clone();
        let notifier = ChannelNotifier::new(channel);

        notifier.notify("hello").await.unwrap();
        assert!(sent.lock().unwrap().iter().any(|m| m.contains("hello")));
    }

    #[tokio::test]
    async fn channel_notifier_proposal_sends_with_buttons() {
        let channel = Arc::new(RecordingChannel {
            sent: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let sent = channel.sent.clone();
        let notifier = ChannelNotifier::new(channel);

        notifier
            .notify_proposal("prop-1", "approve this")
            .await
            .unwrap();
        assert!(sent.lock().unwrap().iter().any(|m| m.contains("approve")));
    }

    #[tokio::test]
    async fn channel_notifier_permission_sends_with_buttons() {
        let channel = Arc::new(RecordingChannel {
            sent: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let sent = channel.sent.clone();
        let notifier = ChannelNotifier::new(channel);

        notifier
            .notify_permission_request("perm-1", "needs access")
            .await
            .unwrap();
        assert!(
            sent.lock()
                .unwrap()
                .iter()
                .any(|m| m.contains("needs access"))
        );
    }

    #[test]
    fn from_env_reads_both_telegram_vars_and_is_none_when_a_var_is_missing() {
        // One test pins the production env entry point (repo convention). Two tests that
        // set/clear the same process-global vars race on Windows CI.
        unsafe { std::env::set_var("LIBERADO_TELEGRAM_BOT_TOKEN", "tok-1") };
        unsafe { std::env::set_var("LIBERADO_TELEGRAM_CHAT_ID", "42") };
        let constructed = TelegramNotifier::from_env();
        unsafe { std::env::remove_var("LIBERADO_TELEGRAM_BOT_TOKEN") };
        unsafe { std::env::remove_var("LIBERADO_TELEGRAM_CHAT_ID") };

        let n = constructed.expect("both vars set -> Some");
        // The un-injected construction keeps the real Telegram base with the env token.
        assert_eq!(
            n.api_url("sendMessage"),
            "https://api.telegram.org/bottok-1/sendMessage"
        );
        assert!(TelegramNotifier::from_env().is_none());
    }

    #[test]
    fn with_api_base_overrides_send_url_and_poll_tuning_keeps_defaults() {
        let n = TelegramNotifier::new("t", "42").with_api_base("http://localhost:9/bot");
        assert_eq!(n.api_url("getMe"), "http://localhost:9/bott/getMe");
    }

    #[test]
    fn channel_name_is_telegram() {
        assert_eq!(TelegramNotifier::new("t", "42").name(), "Telegram");
    }

    #[test]
    fn notify_error_displays_inner_message() {
        let e = NotifyError::from(MessagingError("boom".into()));
        assert_eq!(e.to_string(), "boom");
    }

    #[tokio::test]
    async fn channel_notifier_exposes_inner_channel() {
        let channel = Arc::new(RecordingChannel {
            sent: Arc::new(std::sync::Mutex::new(Vec::new())),
        });
        let notifier = ChannelNotifier::new(channel);
        assert_eq!(notifier.channel().name(), "test-recording");
    }
}
