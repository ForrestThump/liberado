//! # liberado-telegram-approvals
//!
//! Two Telegram surfaces over one long-poll loop:
//!
//! 1. **Proposal Approve/Reject/Revise** — pure-code frontmatter edits (Approve/Reject never
//!    touch an LLM). The two-way half of `liberado-notify`'s `TelegramNotifier::notify_proposal`.
//! 2. **Free-form chat** (optional) — when a [`TelegramChatSurface`] is attached, ordinary
//!    messages from the configured chat become Liberado chat turns. While inference runs the bot
//!    sends Telegram `sendChatAction(typing)` so the user sees "agent is typing…".
//!
//! Free-form chat is opt-in via [`ApprovalBot::with_chat`]: without it, non-revision messages are
//! ignored (the historical behaviour). Only the configured `LIBERADO_TELEGRAM_CHAT_ID` is served.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use liberado_common::{
    PROPOSALS_DIR, Proposal, ProposalSigner, ProposalStatus, ProposedAction, WriteProvenance,
};
use liberado_config_loader::TelegramApprovalsTuning;
use liberado_provider::{CompletionRequest, Message, Provider, complete_json};
use liberado_vault::Vault;
use tokio::sync::Mutex;

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";
/// Telegram hard-caps message text at 4096 UTF-16 code units; stay under with headroom.
const TELEGRAM_MAX_MESSAGE_CHARS: usize = 4000;
/// `sendChatAction` "typing" lasts ~5s on Telegram's side; refresh before it expires.
const TYPING_REFRESH_SECS: u64 = 4;

/// One conversational turn for free-form Telegram messages. Implemented by the server against
/// [`liberado_main_agent::ChatSessions`] so Telegram shares the same face agent + tools as HTTP chat.
#[async_trait]
pub trait TelegramChatSurface: Send + Sync {
    async fn reply(&self, user_text: &str) -> Result<String, String>;
}

/// A Telegram bot that answers Approve/Reject/Revise taps on proposal notifications, and
/// optionally free-form chat when a [`TelegramChatSurface`] is attached.
pub struct ApprovalBot {
    client: reqwest::Client,
    token: String,
    chat_id: String,
    vault: Vault,
    signer: ProposalSigner,
    provider: Arc<dyn Provider>,
    tuning: TelegramApprovalsTuning,
    /// Telegram message_id (of a `force_reply` prompt) → the proposal stem it's revising. Lost on
    /// restart — acceptable, a human can just tap Revise again.
    pending_revisions: Mutex<HashMap<i64, String>>,
    /// When set, free-form messages from `chat_id` run a Liberado chat turn.
    chat: Option<Arc<dyn TelegramChatSurface>>,
    /// Shared "last time the human sent us a message" clock. When set, every inbound message from the
    /// configured chat stamps it `Some(now)`. The server's chat-delivering notifier reads it to hold a
    /// cron brief until you are between messages (see `docs/ideas/cron-delivery-timing-idea.md`). The
    /// inner `None` means "never active" → briefs deliver immediately. The outer `None` means no
    /// delivery timing is being tracked at all — the historical behaviour.
    last_activity: Option<Arc<Mutex<Option<std::time::Instant>>>>,
    /// `(command, description)` pairs (no leading `/`) registered with Telegram's `setMyCommands` on
    /// startup, so typing `/` shows an autocomplete menu. Empty → nothing registered.
    command_menu: Vec<(String, String)>,
}

impl ApprovalBot {
    /// Build from `LIBERADO_TELEGRAM_BOT_TOKEN` + `LIBERADO_TELEGRAM_CHAT_ID` — the same env
    /// vars `TelegramNotifier::from_env` uses (one bot, two halves). `None` when either is unset;
    /// approvals stay Obsidian/TUI-only, same as today. `tuning` is `config.tuning.telegram_approvals`
    /// — pass [`TelegramApprovalsTuning::default()`] to accept the specced defaults.
    ///
    /// Liberado-prefixed on purpose so we do not collide with OpenClaw's `TELEGRAM_BOT_TOKEN` /
    /// `TELEGRAM_CHAT_ID` on a shared host.
    pub fn from_env(
        vault: Vault,
        signer: ProposalSigner,
        provider: Arc<dyn Provider>,
        tuning: TelegramApprovalsTuning,
    ) -> Option<Self> {
        let token = std::env::var("LIBERADO_TELEGRAM_BOT_TOKEN").ok()?;
        let chat_id = std::env::var("LIBERADO_TELEGRAM_CHAT_ID").ok()?;
        Some(Self {
            client: reqwest::Client::new(),
            token,
            chat_id,
            vault,
            signer,
            provider,
            tuning,
            pending_revisions: Mutex::new(HashMap::new()),
            chat: None,
            last_activity: None,
            command_menu: Vec::new(),
        })
    }

    /// Attach free-form chat handling (Liberado face agent). Without this, ordinary messages
    /// that are not revision replies are ignored.
    pub fn with_chat(mut self, chat: Arc<dyn TelegramChatSurface>) -> Self {
        self.chat = Some(chat);
        self
    }

    /// Share a "last inbound message" clock, stamped `Some(now)` on every message from the configured
    /// chat. The chat-delivering notifier reads it to defer a cron brief around active conversation.
    pub fn with_activity_tracker(mut self, clock: Arc<Mutex<Option<std::time::Instant>>>) -> Self {
        self.last_activity = Some(clock);
        self
    }

    /// Advertise `(command, description)` pairs (no leading `/`) to Telegram, so typing `/` shows an
    /// autocomplete menu. Registered once on [`run`](Self::run) startup via `setMyCommands`.
    pub fn with_command_menu(mut self, commands: Vec<(String, String)>) -> Self {
        self.command_menu = commands;
        self
    }

    /// Register the command menu with Telegram (`setMyCommands`), scoped to the configured chat so it
    /// doesn't leak to other users of the same bot. Best-effort: a failure is logged, not fatal.
    async fn register_slash_commands(&self) {
        if self.command_menu.is_empty() {
            return;
        }
        let commands: Vec<serde_json::Value> = self
            .command_menu
            .iter()
            .map(|(command, description)| serde_json::json!({ "command": command, "description": description }))
            .collect();
        let url = format!("{TELEGRAM_API_BASE}{}/setMyCommands", self.token);
        let body = serde_json::json!({
            "commands": commands,
            // Scope to this chat so the menu appears for the operator without publishing it globally.
            "scope": { "type": "chat", "chat_id": self.chat_id },
        });
        match self.client.post(&url).json(&body).send().await {
            Ok(r) if r.status().is_success() => {
                tracing::info!(count = self.command_menu.len(), "registered Telegram slash-command menu");
            }
            Ok(r) => tracing::warn!(status = %r.status(), "setMyCommands non-success"),
            Err(e) => tracing::warn!(error = %e, "setMyCommands failed"),
        }
    }

    /// Long-poll Telegram's `getUpdates` forever, dispatching each `callback_query`/`message` it
    /// sees. Never returns under normal operation — intended to be `tokio::spawn`ed alongside the
    /// daemon's own watch loop.
    pub async fn run(self) {
        tracing::info!(
            chat = self.chat.is_some(),
            "starting Telegram bot poll loop (approvals{})",
            if self.chat.is_some() {
                " + free-form chat"
            } else {
                ""
            }
        );
        // Publish the slash-command autocomplete menu (typing `/` in Telegram lists these).
        self.register_slash_commands().await;
        let mut offset: i64 = 0;
        loop {
            let updates = self.fetch_updates(offset).await;
            for update in &updates {
                let update_id = update["update_id"].as_i64().unwrap_or(0);
                offset = offset.max(update_id + 1);

                if let Some(cq) = update.get("callback_query") {
                    self.handle_callback_query(cq).await;
                } else if let Some(msg) = update.get("message") {
                    self.handle_message(msg).await;
                }
            }
        }
    }

    async fn fetch_updates(&self, offset: i64) -> Vec<serde_json::Value> {
        let getupdate_timeout = self.tuning.getupdate_timeout_secs;
        let url = format!(
            "{TELEGRAM_API_BASE}{token}/getUpdates?offset={offset}&timeout={getupdate_timeout}\
             &allowed_updates=[\"message\",\"callback_query\"]",
            token = self.token,
        );
        match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(getupdate_timeout + 5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["result"].as_array().cloned())
                .unwrap_or_default(),
            Ok(r) => {
                tracing::warn!(status = %r.status(), "getUpdates non-success");
                tokio::time::sleep(Duration::from_secs(self.tuning.poll_retry_backoff_secs)).await;
                vec![]
            }
            Err(e) => {
                tracing::warn!("getUpdates error: {e}");
                tokio::time::sleep(Duration::from_secs(self.tuning.poll_retry_backoff_secs)).await;
                vec![]
            }
        }
    }

    async fn answer_callback_query(&self, callback_query_id: &str, text: &str) {
        let url = format!("{TELEGRAM_API_BASE}{}/answerCallbackQuery", self.token);
        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "callback_query_id": callback_query_id, "text": text }))
            .send()
            .await;
    }

    async fn send_message(&self, text: &str) {
        for chunk in split_telegram_chunks(text) {
            if let Err(e) = self.send_message_raw(&chunk).await {
                tracing::warn!(error = %e, "Telegram sendMessage failed");
            }
        }
    }

    async fn send_message_raw(&self, text: &str) -> Result<(), String> {
        let url = format!("{TELEGRAM_API_BASE}{}/sendMessage", self.token);
        let response = self
            .client
            .post(&url)
            .json(&serde_json::json!({ "chat_id": self.chat_id, "text": text }))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Telegram API: {body}"));
        }
        Ok(())
    }

    /// Show "agent is typing…" in the chat. Telegram expires the indicator after ~5s; call again
    /// to refresh while inference is still running.
    async fn send_typing(&self) {
        let url = format!("{TELEGRAM_API_BASE}{}/sendChatAction", self.token);
        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "action": "typing"
            }))
            .send()
            .await;
    }

    /// Background task that refreshes `sendChatAction(typing)` until aborted. Call
    /// [`send_typing`](Self::send_typing) once first so the indicator shows immediately.
    fn spawn_typing_pulse(&self) -> tokio::task::JoinHandle<()> {
        let client = self.client.clone();
        let token = self.token.clone();
        let chat_id = self.chat_id.clone();
        tokio::spawn(async move {
            loop {
                let url = format!("{TELEGRAM_API_BASE}{token}/sendChatAction");
                let _ = client
                    .post(&url)
                    .json(&serde_json::json!({
                        "chat_id": chat_id,
                        "action": "typing"
                    }))
                    .send()
                    .await;
                tokio::time::sleep(Duration::from_secs(TYPING_REFRESH_SECS)).await;
            }
        })
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

    /// Send `text` with a fresh Approve/Revise/Reject button row for `stem` — used both for the
    /// initial notification (via `liberado-notify`'s `TelegramNotifier`, a separate crate) and
    /// here, after a revision, so the human reviews the redraft before re-approving.
    async fn send_approval_buttons(&self, stem: &str, text: &str) {
        let url = format!("{TELEGRAM_API_BASE}{}/sendMessage", self.token);
        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "reply_markup": {
                    "inline_keyboard": [[
                        { "text": "✅ Approve", "callback_data": format!("approve:{stem}") },
                        { "text": "📝 Revise", "callback_data": format!("revise:{stem}") },
                        { "text": "❌ Reject", "callback_data": format!("reject:{stem}") }
                    ]]
                }
            }))
            .send()
            .await;
    }

    /// Send a `force_reply` prompt so the human's next message is captured as a revision note.
    /// Returns the sent message's id (needed to correlate the reply back to `stem`).
    async fn send_force_reply(&self, text: &str) -> Option<i64> {
        let url = format!("{TELEGRAM_API_BASE}{}/sendMessage", self.token);
        let response = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": self.chat_id,
                "text": text,
                "reply_markup": {
                    "force_reply": true,
                    "input_field_placeholder": "Describe the changes needed..."
                }
            }))
            .send()
            .await
            .ok()?;
        response
            .json::<serde_json::Value>()
            .await
            .ok()?
            .get("result")?
            .get("message_id")?
            .as_i64()
    }

    async fn handle_callback_query(&self, cq: &serde_json::Value) {
        let cq_id = cq["id"].as_str().unwrap_or("");
        let data = cq["data"].as_str().unwrap_or("");

        let Some((action, stem)) = parse_callback_data(data) else {
            tracing::warn!(data, "unexpected callback_data format");
            return;
        };

        match action {
            "approve" => self.set_status(cq_id, stem, ProposalStatus::Approved).await,
            "reject" => self.set_status(cq_id, stem, ProposalStatus::Rejected).await,
            "revise" => self.begin_revision(cq_id, stem).await,
            _ => tracing::warn!(action, "unknown callback action"),
        }
    }

    /// Read `proposals/{stem}.md`, and — only if it is currently `Pending` and not expired — set
    /// its status and write it back tagged as a human write. Any other current state (already
    /// approved/rejected/expired/done, or an unparseable note) is reported back to the human and
    /// left untouched, mirroring the same guards `Daemon::handle_proposal_change` itself checks.
    async fn set_status(&self, cq_id: &str, stem: &str, new_status: ProposalStatus) {
        let path = proposal_path(stem);

        let content = match self.vault.read(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal not found");
                self.answer_callback_query(cq_id, "Proposal not found.")
                    .await;
                return;
            }
        };

        let mut proposal = match Proposal::from_note(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal note did not parse");
                self.answer_callback_query(cq_id, "Could not parse that proposal.")
                    .await;
                return;
            }
        };

        let expired = proposal.is_expired_at(chrono::Utc::now());
        if proposal.status != ProposalStatus::Pending || expired {
            let note = if expired {
                "expired".to_string()
            } else {
                format!("{:?}", proposal.status)
            };
            self.answer_callback_query(cq_id, &format!("Already {note} — no action taken."))
                .await;
            return;
        }

        proposal.status = new_status;
        if let Err(e) = self
            .vault
            .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
            .await
        {
            tracing::error!(stem, error = %e, "approval-bot: failed to write status change");
            self.answer_callback_query(cq_id, "Failed to save — try again.")
                .await;
            return;
        }

        let verb = match new_status {
            ProposalStatus::Approved => "Approved",
            ProposalStatus::Rejected => "Rejected",
            _ => "Updated",
        };
        self.answer_callback_query(cq_id, verb).await;
        self.send_message(&format!("{verb}: {}", proposal.rationale))
            .await;
    }

    /// Tapped Revise: prompt for a free-text note and remember which proposal it belongs to.
    /// Doesn't touch the proposal itself yet — that happens once the reply arrives
    /// ([`apply_revision`](Self::apply_revision)).
    async fn begin_revision(&self, cq_id: &str, stem: &str) {
        self.answer_callback_query(cq_id, "Awaiting your revision note...")
            .await;
        let prompt = format!("Reply to this message with the changes you want for `{stem}`.");
        if let Some(msg_id) = self.send_force_reply(&prompt).await {
            self.pending_revisions
                .lock()
                .await
                .insert(msg_id, stem.to_string());
        } else {
            tracing::warn!(stem, "approval-bot: failed to send force_reply prompt");
        }
    }

    /// Revision replies (threaded to a `force_reply` prompt) update proposals; any other
    /// free-form text from the allowed chat runs a Liberado chat turn when a surface is attached.
    async fn handle_message(&self, msg: &serde_json::Value) {
        // Ignore own/other bots (including ourselves if Telegram echoes).
        if msg
            .get("from")
            .and_then(|f| f.get("is_bot"))
            .and_then(|b| b.as_bool())
            == Some(true)
        {
            return;
        }

        if !self.message_is_from_allowed_chat(msg) {
            tracing::debug!(
                chat = ?msg.get("chat").and_then(|c| c.get("id")),
                "Telegram message from non-configured chat — ignored"
            );
            return;
        }

        // The human just messaged us — stamp the shared activity clock so a pending cron brief holds
        // off until this conversation goes quiet (any inbound message counts, chat or revision).
        if let Some(clock) = &self.last_activity {
            *clock.lock().await = Some(std::time::Instant::now());
        }

        // Revision path: reply to one of our force_reply prompts.
        if let Some(reply_to_id) = msg
            .get("reply_to_message")
            .and_then(|r| r["message_id"].as_i64())
        {
            let stem = { self.pending_revisions.lock().await.remove(&reply_to_id) };
            if let Some(stem) = stem {
                let note = msg["text"].as_str().unwrap_or("").trim().to_string();
                if note.is_empty() {
                    self.send_message("Revision note was empty — please try again.")
                        .await;
                    return;
                }
                self.send_typing().await;
                let pulse = self.spawn_typing_pulse();
                self.apply_revision(&stem, &note).await;
                pulse.abort();
                return;
            }
        }

        let text = msg["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            // Stickers, photos, etc. without a caption — acknowledge so it doesn't feel dead.
            self.send_message("I only handle text messages for now.")
                .await;
            return;
        }

        let Some(chat) = self.chat.as_ref() else {
            tracing::info!(
                "Telegram free-form message received but no chat surface attached — ignored"
            );
            return;
        };

        if text == "/start" || text == "/help" {
            self.send_message(
                "Liberado is online. Send a normal message to chat. \
                 Proposal Approve/Revise/Reject buttons still work as before.",
            )
            .await;
            return;
        }

        tracing::info!(len = text.len(), "Telegram chat message received");
        // Typing immediately so the user knows we got it before inference starts.
        self.send_typing().await;
        let pulse = self.spawn_typing_pulse();
        let outcome = chat.reply(text).await;
        pulse.abort();
        match outcome {
            Ok(reply) => {
                let reply = reply.trim();
                if reply.is_empty() {
                    self.send_message("(no reply text)").await;
                } else {
                    self.send_message(reply).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Telegram chat turn failed");
                self.send_message(&format!("Sorry — that turn failed: {e}"))
                    .await;
            }
        }
    }

    /// Ask the shared provider to redraft `stem`'s `rationale`/`proposed_action` per `note`, then
    /// write the result back as a **fresh, re-signed, still-Pending** proposal and send new
    /// buttons. Never auto-approves — see the module doc comment.
    async fn apply_revision(&self, stem: &str, note: &str) {
        let path = proposal_path(stem);

        let content = match self.vault.read(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal not found for revision");
                self.send_message("Could not find that proposal.").await;
                return;
            }
        };

        let mut proposal = match Proposal::from_note(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal note did not parse");
                self.send_message("Could not parse that proposal.").await;
                return;
            }
        };

        if proposal.status != ProposalStatus::Pending {
            self.send_message(&format!(
                "Proposal is already {:?} — cannot revise.",
                proposal.status
            ))
            .await;
            return;
        }

        let request = build_revision_request(&proposal, note, self.tuning.revise_temperature);
        let revision: ProposalRevision =
            match complete_json(self.provider.as_ref(), request, revision_schema()).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(stem, error = %e, "approval-bot: revision LLM call failed");
                    self.send_message(&format!(
                        "Could not apply that revision ({e}) — the proposal is unchanged."
                    ))
                    .await;
                    return;
                }
            };

        // proposed_action is a signed field (see ProposalSigner::compute) — any revision, even a
        // no-op one, must get a fresh signature. status stays Pending: only a subsequent Approve
        // tap (pure code) can ever execute this.
        proposal.rationale = revision.rationale;
        proposal.proposed_action = revision.proposed_action;
        let mut proposal = self.signer.sign(proposal);
        proposal.set_status(ProposalStatus::Pending);

        if let Err(e) = self
            .vault
            .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
            .await
        {
            tracing::error!(stem, error = %e, "approval-bot: failed to write revision");
            self.send_message("Failed to save the revision — try again.")
                .await;
            return;
        }

        self.send_approval_buttons(
            stem,
            &format!(
                "Revised — please review before approving:\n{}",
                proposal.rationale
            ),
        )
        .await;
    }
}

/// What the provider is asked to return for a revision: a redrafted rationale plus (possibly
/// edited) proposed action. Reuses [`ProposedAction`]'s own `Deserialize` directly — if the model
/// doesn't reproduce its shape, `complete_json` surfaces a decode error and the revision fails
/// safely (the proposal file is left untouched).
#[derive(serde::Deserialize)]
struct ProposalRevision {
    rationale: String,
    proposed_action: ProposedAction,
}

/// Pure — the vault-relative path for a proposal's filename stem. Matches the convention both
/// `Daemon::write_proposal` and `RiskGatedToolRuntime::write_proposal` already write to.
fn proposal_path(stem: &str) -> String {
    format!("{PROPOSALS_DIR}/{stem}.md")
}

/// Pure — split Telegram `callback_data` of the form `"{action}:{stem}"` into its parts.
/// `split_once` (not a full split) is deliberate: a proposal stem is itself dash-only (see
/// `liberado_daemon`'s `slugify`), never containing `:`, but splitting on the *first* colon only
/// is what makes this robust even if that ever changed.
fn parse_callback_data(data: &str) -> Option<(&str, &str)> {
    data.split_once(':')
}

/// Pure — the `CompletionRequest` for a revision call: the current rationale/action as a concrete
/// worked example, plus the human's free-text note. `temperature` is
/// `config.tuning.telegram_approvals.revise_temperature` (0 by default, for a faithful,
/// non-creative edit rather than an unrelated rewrite).
fn build_revision_request(proposal: &Proposal, note: &str, temperature: f32) -> CompletionRequest {
    let current_action = serde_json::to_string_pretty(&proposal.proposed_action)
        .unwrap_or_else(|_| "{}".to_string());
    let system = "You are revising a pending Liberado proposal per a human's free-text request. \
        You will be shown the proposal's current rationale and its proposed_action as JSON. \
        Return an updated rationale and an updated proposed_action that preserves the exact same \
        JSON shape/keys as the example — only change the values the human's request implies. If \
        the request doesn't change the underlying action, return proposed_action unchanged.";
    let user = format!(
        "Current rationale: {}\n\nCurrent proposed_action (JSON):\n{current_action}\n\n\
         Human's requested changes: {note}",
        proposal.rationale,
    );
    CompletionRequest::new(vec![Message::system(system), Message::user(user)])
        .with_temperature(temperature)
}

/// Pure — a loose JSON schema for [`ProposalRevision`] (the prompt carries the exact shape via the
/// worked example in [`build_revision_request`], same "the prompt carries the shape" precedent
/// `liberado-dispatcher`'s own schema uses — no `schemars` dependency exists in this codebase).
fn revision_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "rationale": { "type": "string" },
            "proposed_action": {}
        },
        "required": ["rationale", "proposed_action"]
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::{CompletionResponse, MockProvider};
    use tempfile::TempDir;

    #[test]
    fn proposal_path_joins_the_stem() {
        assert_eq!(proposal_path("prop-1"), "proposals/prop-1.md");
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
    fn parse_callback_data_splits_on_first_colon_only() {
        assert_eq!(
            parse_callback_data("approve:prop-1"),
            Some(("approve", "prop-1"))
        );
        // A stem is dash-only in practice, but split_once proves this is robust even if it weren't.
        assert_eq!(
            parse_callback_data("reject:vault-change-inbox-x-md-abc"),
            Some(("reject", "vault-change-inbox-x-md-abc"))
        );
    }

    #[test]
    fn parse_callback_data_rejects_malformed_input() {
        assert_eq!(parse_callback_data("no-colon-here"), None);
    }

    #[test]
    fn revision_schema_requires_rationale_and_proposed_action() {
        let schema = revision_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("rationale")));
        assert!(required.contains(&serde_json::json!("proposed_action")));
    }

    #[test]
    fn build_revision_request_carries_the_current_action_and_the_note() {
        let proposal = Proposal::pending(
            "prop-1",
            "corr-1",
            "liberado",
            ProposedAction::External {
                description: "send an email".into(),
            },
            "original rationale",
        );
        let request =
            build_revision_request(&proposal, "send it to a different address instead", 0.0);

        let user_msg = &request.messages[1];
        assert!(user_msg.content.contains("original rationale"));
        assert!(user_msg.content.contains("send an email"));
        assert!(
            user_msg
                .content
                .contains("send it to a different address instead")
        );
        assert_eq!(request.temperature, Some(0.0));
    }

    #[test]
    fn build_revision_request_honors_the_configured_temperature() {
        let proposal = Proposal::pending(
            "prop-1",
            "corr-1",
            "liberado",
            ProposedAction::External {
                description: "send an email".into(),
            },
            "original rationale",
        );
        let request = build_revision_request(&proposal, "a note", 0.4);
        assert_eq!(request.temperature, Some(0.4));
    }

    fn test_bot(vault: Vault, signer: ProposalSigner, provider: Arc<dyn Provider>) -> ApprovalBot {
        ApprovalBot {
            client: reqwest::Client::new(),
            token: "unused".into(),
            chat_id: "unused".into(),
            vault,
            signer,
            provider,
            tuning: TelegramApprovalsTuning::default(),
            pending_revisions: Mutex::new(HashMap::new()),
            chat: None,
            last_activity: None,
            command_menu: Vec::new(),
        }
    }

    async fn temp_vault_with_proposal(
        signer: &ProposalSigner,
        status: ProposalStatus,
    ) -> (Vault, TempDir, String) {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();

        let proposal = Proposal::pending(
            "prop-1",
            "corr-1",
            "liberado",
            ProposedAction::External {
                description: "send an email".into(),
            },
            "a test proposal",
        );
        let mut proposal = signer.sign(proposal);
        proposal.set_status(status);
        vault
            .write(
                "proposals/prop-1.md",
                &proposal.to_note(),
                None,
                &WriteProvenance::human(),
            )
            .await
            .unwrap();

        (vault, dir, "prop-1".to_string())
    }

    #[tokio::test]
    async fn approving_a_pending_proposal_flips_its_status() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.set_status("cq-1", &stem, ProposalStatus::Approved)
            .await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }

    #[tokio::test]
    async fn rejecting_an_already_approved_proposal_is_a_no_op() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Approved).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.set_status("cq-1", &stem, ProposalStatus::Rejected)
            .await;

        // Still Approved — the guard must refuse to touch a non-Pending proposal.
        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }

    #[tokio::test]
    async fn a_human_write_is_reacted_to_not_suppressed() {
        // The whole point of WriteProvenance::human() — proves this crate's writes will actually
        // be observed by the daemon's vault watcher, not silently loop-broken like an agent write.
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));
        bot.set_status("cq-1", &stem, ProposalStatus::Approved)
            .await;

        assert_eq!(
            vault.attribute("proposals/prop-1.md").await.unwrap(),
            liberado_vault::Attribution::External
        );
    }

    #[tokio::test]
    async fn revising_a_pending_proposal_rewrites_it_resigns_and_stays_pending() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;

        let revised = serde_json::json!({
            "rationale": "send a follow-up email to the new address",
            "proposed_action": { "External": { "description": "send an email to boss2@example.com" } }
        });
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text(revised.to_string())],
        ));
        let bot = test_bot(vault.clone(), signer.clone(), provider);

        bot.apply_revision(&stem, "actually send it to boss2@example.com instead")
            .await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Pending);
        assert_eq!(
            proposal.rationale,
            "send a follow-up email to the new address"
        );
        assert!(signer.verify(&proposal), "the revision must be re-signed");
        match proposal.proposed_action {
            ProposedAction::External { description } => {
                assert_eq!(description, "send an email to boss2@example.com");
            }
            other => panic!("expected External, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_revision_that_fails_to_decode_leaves_the_proposal_untouched() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;

        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("not valid json for our schema")],
        ));
        let bot = test_bot(vault.clone(), signer.clone(), provider);

        bot.apply_revision(&stem, "some change").await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Pending);
        assert_eq!(proposal.rationale, "a test proposal");
    }

    #[tokio::test]
    async fn revising_a_non_pending_proposal_is_a_no_op() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Approved).await;
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("{}")],
        ));
        let bot = test_bot(vault.clone(), signer, provider);

        bot.apply_revision(&stem, "some change").await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }
}
