//! # liberado-messaging
//!
//! Channel-agnostic traits for human chat clients Liberado talks to. Telegram is the first
//! implementation (in `liberado-notify` / `liberado-telegram-approvals`); Matrix, Signal, Discord,
//! and similar clients plug in by implementing [`MessagingChannel`] — no changes to the approval
//! bot or the face-agent chat bridge.
//!
//! ## Two seams
//!
//! | Trait | Direction | Role |
//! |---|---|---|
//! | [`MessagingChannel`] | duplex transport | send text/actions, receive inbound events |
//! | [`ChatSurface`] | inbound free-form → Liberado | one chat turn (slash commands + face agent) |
//!
//! `liberado_notify::Notifier` remains the *one-way unattended* notify seam (cron/proposal
//! pings). A channel can implement both, or a thin adapter can wrap any [`MessagingChannel`] as a
//! `Notifier` (see `liberado_notify::ChannelNotifier`).

use async_trait::async_trait;

/// Free-form chat handler: a channel-agnostic turn into Liberado's face agent.
///
/// Implemented by the server (sticky session + slash commands). Attached to the approval/chat bot
/// so ordinary messages become Liberado turns. Without it, free-form text is ignored.
#[async_trait]
pub trait ChatSurface: Send + Sync {
    async fn reply(&self, user_text: &str) -> Result<String, String>;
}

/// One interactive button on an outbound message (Approve, Deny, Once, …).
///
/// Channels encode this into their native action payload (Telegram `callback_data`, Discord
/// `custom_id`, …). Callers never build channel-specific wire formats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionButton {
    /// Human-visible label (may include emoji).
    pub label: String,
    /// Stable action verb: `approve`, `reject`, `revise`, `once`, `session`, `everywhere`, `deny`.
    pub action: String,
    /// Correlation id the tap must act on (proposal stem, permission-request id, …).
    pub correlation_id: String,
}

impl ActionButton {
    pub fn new(
        label: impl Into<String>,
        action: impl Into<String>,
        correlation_id: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            action: action.into(),
            correlation_id: correlation_id.into(),
        }
    }
}

/// Something a human did on a messaging channel that Liberado should handle.
#[derive(Debug, Clone)]
pub enum InboundEvent {
    /// Free-form text (or a reply to a prior [`MessagingChannel::request_reply`] prompt).
    Message {
        text: String,
        /// When set, this message is a reply to a prompt we issued (e.g. a revision note).
        reply_to_prompt: Option<String>,
        /// True when the sender is a bot (including ourselves). Callers usually ignore these.
        from_bot: bool,
    },
    /// Interactive action button tap.
    Action {
        action: String,
        correlation_id: String,
        /// Channel-native id for [`MessagingChannel::acknowledge`] (Telegram `callback_query.id`, …).
        event_id: String,
        /// Channel-native id of the message the button lives on, so a handler can edit it after a
        /// tap — strip the now-stale buttons and stamp a receipt (Telegram
        /// `callback_query.message.message_id`). `None` when the channel doesn't supply one.
        message_ref: Option<String>,
    },
}

/// A messaging transport failed. Callers treat this as best-effort for outbound notify paths;
/// the approval bot logs and retries on receive failures.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MessagingError(pub String);

impl MessagingError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

/// Duplex transport for a human chat client (Telegram, Discord, Matrix, Signal, …).
///
/// Outbound methods send to the configured peer; [`receive`](Self::receive) yields the next batch
/// of inbound events. Channel-specific limits (message length, action-id size) stay inside the
/// implementation — callers pass plain text and [`ActionButton`]s.
#[async_trait]
pub trait MessagingChannel: Send + Sync {
    /// Human-readable channel name for logs and sticky session titles (`"Telegram"`, `"Discord"`, …).
    fn name(&self) -> &str;

    /// Send a plain text message. Implementations may chunk long text to fit channel limits.
    async fn send_text(&self, text: &str) -> Result<(), MessagingError>;

    /// Send text with interactive action buttons arranged in rows.
    ///
    /// Channels that cannot render buttons should fall back to plain [`send_text`](Self::send_text)
    /// (optionally appending action labels as text). Channels with hard action-id size limits may
    /// also fall back rather than failing the send.
    async fn send_with_actions(
        &self,
        text: &str,
        rows: &[Vec<ActionButton>],
    ) -> Result<(), MessagingError>;

    /// Prompt the human for free text (Telegram `force_reply`, Discord modal, …).
    ///
    /// Returns a prompt id that subsequent [`InboundEvent::Message::reply_to_prompt`] values will
    /// carry so the bot can correlate the reply.
    async fn request_reply(&self, prompt: &str) -> Result<String, MessagingError>;

    /// Acknowledge an interactive action (e.g. dismiss a Telegram spinner with a short toast).
    /// Best-effort: a failure is logged by the caller, never fatal.
    async fn acknowledge(&self, event_id: &str, text: &str) -> Result<(), MessagingError>;

    /// Replace a previously-sent message's text and remove its interactive buttons — used to
    /// "receipt" an action tap ("✅ Approved everywhere") so the buttons don't stay live for a
    /// second, now-meaningless tap. `message_ref` is the id carried on the
    /// [`InboundEvent::Action`] the tap produced. Default is a no-op for channels without message
    /// editing (they keep the buttons; harmless — a repeat tap hits the "already decided" guard).
    async fn edit_message(&self, _message_ref: &str, _text: &str) -> Result<(), MessagingError> {
        Ok(())
    }

    /// Show a typing / composing indicator. Default is a no-op for channels that lack one.
    async fn set_typing(&self) -> Result<(), MessagingError> {
        Ok(())
    }

    /// Register slash-command autocomplete entries `(command, description)` without a leading `/`.
    /// Default is a no-op for channels that lack a command menu.
    async fn register_commands(
        &self,
        _commands: &[(String, String)],
    ) -> Result<(), MessagingError> {
        Ok(())
    }

    /// Pull the next batch of inbound events.
    ///
    /// `cursor` is opaque, channel-owned state (e.g. a Telegram `getUpdates` offset as a decimal
    /// string). The channel updates it between calls. Long-poll / websocket drain / sync-token
    /// details stay inside the implementation.
    ///
    /// On transient transport errors the channel may return `Err` (caller backs off) or sleep and
    /// return an empty batch — both are acceptable.
    async fn receive(&self, cursor: &mut String) -> Result<Vec<InboundEvent>, MessagingError>;
}

/// Approve / Revise / Reject button row for a pending proposal.
pub fn approval_action_rows(proposal_id: &str) -> Vec<Vec<ActionButton>> {
    vec![vec![
        ActionButton::new("✅ Approve", "approve", proposal_id),
        ActionButton::new("📝 Revise", "revise", proposal_id),
        ActionButton::new("❌ Reject", "reject", proposal_id),
    ]]
}

/// Permission-request scope buttons (Deny / Once / Session / Everywhere), two rows.
pub fn permission_action_rows(proposal_id: &str) -> Vec<Vec<ActionButton>> {
    vec![
        vec![
            ActionButton::new("✅ Once", "once", proposal_id),
            ActionButton::new("🔁 This session", "session", proposal_id),
        ],
        vec![
            ActionButton::new("♾️ Everywhere", "everywhere", proposal_id),
            ActionButton::new("❌ Deny", "deny", proposal_id),
        ],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_rows_carry_prefixed_actions() {
        let rows = approval_action_rows("prop-1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 3);
        assert_eq!(rows[0][0].action, "approve");
        assert_eq!(rows[0][1].action, "revise");
        assert_eq!(rows[0][2].action, "reject");
        assert!(rows[0].iter().all(|b| b.correlation_id == "prop-1"));
    }

    #[test]
    fn permission_rows_have_four_scope_buttons() {
        let rows = permission_action_rows("perm-1");
        let actions: Vec<&str> = rows.iter().flatten().map(|b| b.action.as_str()).collect();
        assert_eq!(actions, vec!["once", "session", "everywhere", "deny"]);
    }
}
