//! # liberado-telegram-approvals
//!
//! Channel-agnostic approval + free-form chat bot over a [`MessagingChannel`]:
//!
//! 1. **Proposal Approve/Reject/Revise** — pure-code frontmatter edits (Approve/Reject never
//!    touch an LLM). The two-way half of `liberado-notify`'s proposal notifications.
//! 2. **Free-form chat** (optional) — when a [`ChatSurface`] is attached, ordinary messages
//!    become Liberado chat turns. While inference runs the bot pulses the channel's typing
//!    indicator.
//!
//! Free-form chat is opt-in via [`ApprovalBot::with_chat`]: without it, non-revision messages are
//! ignored (the historical behaviour). The default transport is Telegram
//! ([`TelegramChannel`](liberado_notify::TelegramChannel)); Matrix / Signal / Discord plug in by
//! implementing [`MessagingChannel`] and constructing the bot with [`ApprovalBot::new`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use liberado_common::{
    GrantScope, PROPOSALS_DIR, Proposal, ProposalSigner, ProposalStatus, ProposedAction,
    WriteProvenance,
};
use liberado_config_loader::TelegramApprovalsTuning;
use liberado_messaging::approval_action_rows;
use liberado_notify::TelegramNotifier;
use liberado_provider::{CompletionRequest, Message, Provider, complete_json};
use liberado_vault::Vault;
use tokio::sync::Mutex;

// Re-export so composition roots and the chat bridge can depend on one crate for the bot surface.
// `TelegramChatSurface` is the historical name — prefer `ChatSurface` for new code.
pub use liberado_messaging::{
    ActionButton, ChatSurface, ChatSurface as TelegramChatSurface, InboundEvent, MessagingChannel,
};

/// Refresh typing indicators this often while a long turn runs. Telegram's indicator lasts ~5s;
/// other channels no-op `set_typing` so the pulse is harmless.
const TYPING_REFRESH_SECS: u64 = 4;

/// Answers Approve/Reject/Revise (and permission-scope) taps on proposal notifications, and
/// optionally free-form chat when a [`ChatSurface`] is attached.
///
/// Transport is entirely behind [`MessagingChannel`] — Telegram today, any client tomorrow.
pub struct ApprovalBot {
    channel: Arc<dyn MessagingChannel>,
    vault: Vault,
    signer: ProposalSigner,
    provider: Arc<dyn Provider>,
    tuning: TelegramApprovalsTuning,
    /// Where a tap is recorded. The vault note is a view; this is the decision.
    approvals: Option<liberado_common::ApprovalLedger>,
    /// Prompt id (from [`MessagingChannel::request_reply`]) → proposal stem being revised.
    /// Lost on restart — acceptable; a human can tap Revise again.
    pending_revisions: Mutex<HashMap<String, String>>,
    /// When set, free-form messages run a Liberado chat turn.
    chat: Option<Arc<dyn ChatSurface>>,
    /// Shared "last time the human sent us a message" clock. When set, every inbound message
    /// stamps it `Some(now)`. The server's chat-delivering notifier reads it to hold a cron brief
    /// until you are between messages. The inner `None` means "never active" → briefs deliver
    /// immediately. The outer `None` means no delivery timing is being tracked at all.
    last_activity: Option<Arc<Mutex<Option<std::time::Instant>>>>,
    /// `(command, description)` pairs (no leading `/`) registered with the channel on startup.
    command_menu: Vec<(String, String)>,
    /// How many chat turns are running right now.
    ///
    /// Inbound events are handled concurrently (see [`run`](Self::run)), so a long turn no longer
    /// blocks the next message — but "the bot answered you while something else is still going" is
    /// a state the human cannot see, and silently having work in flight is worse than waiting for
    /// it. Read when a message arrives to say so out loud.
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
    /// Sequence number of the most recently *received* chat message.
    ///
    /// Concurrency means replies can land out of order: ask for deep research, then ask something
    /// quick, and the quick answer arrives first with the research following ten minutes later. A
    /// reply whose sequence is no longer the latest gets an explicit "re: …" marker, because two
    /// unlabelled answers in the wrong order are worse than a slow one.
    latest_seq: Arc<std::sync::atomic::AtomicU64>,
}

impl ApprovalBot {
    /// Build over an arbitrary [`MessagingChannel`]. Prefer this when wiring Matrix/Discord/Signal.
    pub fn new(
        channel: Arc<dyn MessagingChannel>,
        vault: Vault,
        signer: ProposalSigner,
        provider: Arc<dyn Provider>,
        tuning: TelegramApprovalsTuning,
    ) -> Self {
        Self {
            channel,
            vault,
            signer,
            provider,
            tuning,
            approvals: None,
            pending_revisions: Mutex::new(HashMap::new()),
            in_flight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            latest_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            chat: None,
            last_activity: None,
            command_menu: Vec::new(),
        }
    }

    /// Build a Telegram-backed bot from `LIBERADO_TELEGRAM_BOT_TOKEN` + `LIBERADO_TELEGRAM_CHAT_ID`.
    /// `None` when either is unset; approvals stay Obsidian/TUI-only. `tuning` is
    /// `config.tuning.telegram_approvals` — pass [`TelegramApprovalsTuning::default()`] for defaults.
    pub fn from_env(
        vault: Vault,
        signer: ProposalSigner,
        provider: Arc<dyn Provider>,
        tuning: TelegramApprovalsTuning,
    ) -> Option<Self> {
        let channel = TelegramNotifier::from_env()?.with_poll_tuning(
            tuning.getupdate_timeout_secs,
            tuning.poll_retry_backoff_secs,
        );
        Some(Self::new(
            Arc::new(channel),
            vault,
            signer,
            provider,
            tuning,
        ))
    }

    /// Attach free-form chat handling (Liberado face agent). Without this, ordinary messages
    /// that are not revision replies are ignored.
    pub fn with_chat(mut self, chat: Arc<dyn ChatSurface>) -> Self {
        self.chat = Some(chat);
        self
    }

    /// Share a "last inbound message" clock, stamped `Some(now)` on every message from the channel.
    /// The chat-delivering notifier reads it to defer a cron brief around active conversation.
    pub fn with_activity_tracker(mut self, clock: Arc<Mutex<Option<std::time::Instant>>>) -> Self {
        self.last_activity = Some(clock);
        self
    }

    /// Advertise `(command, description)` pairs (no leading `/`) for slash-command autocomplete.
    /// Registered once on [`run`](Self::run) startup via the channel.
    pub fn with_command_menu(mut self, commands: Vec<(String, String)>) -> Self {
        self.command_menu = commands;
        self
    }

    /// Poll the channel forever, dispatching each inbound event. Never returns under normal
    /// operation — intended to be `tokio::spawn`ed alongside the daemon's own watch loop.
    pub async fn run(self) {
        tracing::info!(
            channel = self.channel.name(),
            chat = self.chat.is_some(),
            "starting messaging bot poll loop (approvals{})",
            if self.chat.is_some() {
                " + free-form chat"
            } else {
                ""
            }
        );
        if !self.command_menu.is_empty() {
            match self.channel.register_commands(&self.command_menu).await {
                Ok(()) => tracing::info!(
                    count = self.command_menu.len(),
                    channel = self.channel.name(),
                    "registered slash-command menu"
                ),
                Err(e) => tracing::warn!(
                    error = %e,
                    channel = self.channel.name(),
                    "register_commands failed"
                ),
            }
        }

        // Handle each event on its own task. Serially awaiting them meant one long turn froze the
        // whole surface: a deep-research delegate blocks for ~10 minutes, and during that window no
        // other chat, no /status, and — worst — no proposal Approve/Reject tap was processed, since
        // actions arrive through this same loop. The approval path being hostage to an unrelated
        // chat turn is the part that actually mattered.
        let this = Arc::new(self);
        let mut cursor = String::from("0");
        loop {
            match this.channel.receive(&mut cursor).await {
                Ok(events) => {
                    for event in events {
                        let bot = Arc::clone(&this);
                        tokio::spawn(async move { bot.handle_event(event).await });
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        channel = this.channel.name(),
                        "messaging receive error"
                    );
                    tokio::time::sleep(Duration::from_secs(this.tuning.poll_retry_backoff_secs))
                        .await;
                }
            }
        }
    }

    async fn handle_event(&self, event: InboundEvent) {
        match event {
            InboundEvent::Action {
                action,
                correlation_id,
                event_id,
                message_ref,
            } => {
                self.handle_action(&action, &correlation_id, &event_id, message_ref.as_deref())
                    .await
            }
            InboundEvent::Message {
                text,
                reply_to_prompt,
                from_bot,
            } => {
                if from_bot {
                    return;
                }
                self.handle_message(&text, reply_to_prompt).await;
            }
        }
    }

    async fn handle_action(
        &self,
        action: &str,
        stem: &str,
        event_id: &str,
        message_ref: Option<&str>,
    ) {
        match action {
            "approve" => {
                self.set_status(event_id, message_ref, stem, ProposalStatus::Approved)
                    .await
            }
            "reject" => {
                self.set_status(event_id, message_ref, stem, ProposalStatus::Rejected)
                    .await
            }
            "revise" => self.begin_revision(event_id, stem).await,
            "deny" => {
                self.set_permission_scope(event_id, message_ref, stem, None)
                    .await
            }
            "once" => {
                self.set_permission_scope(event_id, message_ref, stem, Some(GrantScope::Once))
                    .await
            }
            "session" => {
                self.set_permission_scope(event_id, message_ref, stem, Some(GrantScope::Session))
                    .await
            }
            "everywhere" => {
                self.set_permission_scope(event_id, message_ref, stem, Some(GrantScope::Everywhere))
                    .await
            }
            _ => tracing::warn!(action, "unknown messaging action"),
        }
    }

    /// Record approvals to `ledger` — the daemon will not execute without a matching entry.
    #[must_use]
    pub fn with_approval_ledger(mut self, ledger: liberado_common::ApprovalLedger) -> Self {
        self.approvals = Some(ledger);
        self
    }

    /// Which terminal state a proposal was archived into, if it was.
    ///
    /// The daemon files a finished proposal under `proposals/archive/<outcome>/`, so an absent
    /// active note usually means *resolved*, not *missing*. Distinguishing them is what lets a
    /// stale tap get a true answer instead of an alarming one.
    async fn archived_outcome(&self, stem: &str) -> Option<&'static str> {
        for outcome in ["approved", "rejected", "expired"] {
            let path = format!("{PROPOSALS_DIR}/archive/{outcome}/{stem}.md");
            if self.vault.read(&path).await.is_ok() {
                return Some(outcome);
            }
        }
        None
    }

    async fn ack(&self, event_id: &str, text: &str) {
        if let Err(e) = self.channel.acknowledge(event_id, text).await {
            tracing::warn!(error = %e, "acknowledge failed");
        }
    }

    async fn send_text(&self, text: &str) {
        if let Err(e) = self.channel.send_text(text).await {
            tracing::warn!(error = %e, channel = self.channel.name(), "send_text failed");
        }
    }

    /// Stamp a decision receipt: edit the tapped message to `body` and strip its now-stale buttons.
    /// Falls back to a fresh message when the channel gave us no message ref or editing failed, so
    /// the human always sees the outcome.
    async fn receipt(&self, message_ref: Option<&str>, body: &str) {
        match message_ref {
            Some(mref) => {
                if let Err(e) = self.channel.edit_message(mref, body).await {
                    tracing::warn!(error = %e, "approval-bot: edit_message receipt failed; sending plain text");
                    self.send_text(body).await;
                }
            }
            None => self.send_text(body).await,
        }
    }

    /// Background task that refreshes the typing indicator until aborted.
    fn spawn_typing_pulse(&self) -> tokio::task::JoinHandle<()> {
        let channel = self.channel.clone();
        tokio::spawn(async move {
            loop {
                let _ = channel.set_typing().await;
                tokio::time::sleep(Duration::from_secs(TYPING_REFRESH_SECS)).await;
            }
        })
    }

    /// Handle a permission-request scope tap. `scope = None` denies (Rejected); otherwise stamp the
    /// chosen [`GrantScope`] and approve. Same pending/expired guards as [`set_status`]; the daemon's
    /// proposal reactor does the privileged work (apply the grant, execute the carried call).
    async fn set_permission_scope(
        &self,
        event_id: &str,
        message_ref: Option<&str>,
        stem: &str,
        scope: Option<GrantScope>,
    ) {
        let path = proposal_path(stem);
        let content = match self.vault.read(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: permission request not found");
                self.ack(event_id, "Request not found.").await;
                return;
            }
        };
        let mut proposal = match Proposal::from_note(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: permission note did not parse");
                self.ack(event_id, "Could not parse that request.").await;
                return;
            }
        };
        if proposal.requested_grant.is_none() {
            self.ack(event_id, "Not a permission request.").await;
            return;
        }
        if proposal.status != ProposalStatus::Pending || proposal.is_expired_at(chrono::Utc::now())
        {
            self.ack(event_id, "Already decided — no action taken.")
                .await;
            return;
        }

        match scope {
            None => proposal.status = ProposalStatus::Rejected,
            Some(s) => {
                proposal.approved_scope = Some(s);
                proposal.status = ProposalStatus::Approved;
            }
        }
        if let Err(e) = self
            .vault
            .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
            .await
        {
            tracing::error!(stem, error = %e, "approval-bot: failed to write permission decision");
            self.ack(event_id, "Failed to save — try again.").await;
            return;
        }

        let (icon, verb) = match scope {
            None => ("❌", "Denied"),
            Some(GrantScope::Once) => ("✅", "Approved once"),
            Some(GrantScope::Session) => ("🔁", "Approved for this session"),
            Some(GrantScope::Everywhere) => ("♾️", "Approved everywhere"),
        };
        self.ack(event_id, verb).await;
        self.receipt(
            message_ref,
            &format!("{icon} {verb} — {}", proposal.rationale),
        )
        .await;
    }

    /// Read `proposals/{stem}.md`, and — only if it is currently `Pending` and not expired — set
    /// its status and write it back tagged as a human write. Any other current state is reported
    /// back to the human and left untouched.
    async fn set_status(
        &self,
        event_id: &str,
        message_ref: Option<&str>,
        stem: &str,
        new_status: ProposalStatus,
    ) {
        let path = proposal_path(stem);

        let content = match self.vault.read(&path).await {
            Ok(c) => c,
            Err(e) => {
                // The common case is not a missing file: the daemon archives a proposal the moment
                // it reaches a terminal state, so a second tap on a notification that is still on
                // screen reads the active dir and finds nothing. Reporting that as a vault I/O
                // error sent a real debugging session (2026-08-01) looking for a storage fault that
                // did not exist, and told the operator "Proposal not found" about a proposal that
                // had been approved seconds earlier.
                match self.archived_outcome(stem).await {
                    Some(outcome) => {
                        tracing::info!(
                            stem,
                            outcome,
                            "approval-bot: proposal already resolved and archived; nothing to do"
                        );
                        self.ack(event_id, &format!("Already {outcome}.")).await;
                    }
                    None => {
                        tracing::warn!(stem, error = %e, "approval-bot: proposal not found");
                        self.ack(event_id, "Proposal not found.").await;
                    }
                }
                return;
            }
        };

        let mut proposal = match Proposal::from_note(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal note did not parse");
                self.ack(event_id, "Could not parse that proposal.").await;
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
            self.ack(event_id, &format!("Already {note} — no action taken."))
                .await;
            return;
        }

        // The decision is recorded **before** the note is touched, and the note is only a view.
        // A tap is the authenticated act; `proposals/` is agent-writable, so nothing written there
        // authorises anything. If the ledger write fails, the decision did not happen — say so
        // rather than leaving a note that claims otherwise.
        if let Some(ledger) = &self.approvals {
            let decision = match new_status {
                ProposalStatus::Approved => Some(liberado_common::ApprovalDecision::Approved),
                ProposalStatus::Rejected => Some(liberado_common::ApprovalDecision::Rejected),
                _ => None,
            };
            if let Some(decision) = decision
                && let Err(e) = ledger.record(&proposal.id, decision, "telegram").await
            {
                tracing::error!(stem, error = %e, "approval-bot: failed to record the decision");
                self.ack(event_id, "Failed to record your decision — try again.")
                    .await;
                return;
            }
        }

        proposal.status = new_status;
        if let Err(e) = self
            .vault
            .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
            .await
        {
            // The decision stands — it is in the ledger. Only the human-readable view failed.
            tracing::error!(stem, error = %e, "approval-bot: failed to write status change");
            self.ack(event_id, "Failed to save — try again.").await;
            return;
        }

        let (icon, verb) = match new_status {
            ProposalStatus::Approved => ("✅", "Approved"),
            ProposalStatus::Rejected => ("❌", "Rejected"),
            _ => ("✏️", "Updated"),
        };
        self.ack(event_id, verb).await;
        self.receipt(
            message_ref,
            &format!("{icon} {verb} — {}", proposal.rationale),
        )
        .await;
    }

    /// Tapped Revise: prompt for a free-text note and remember which proposal it belongs to.
    async fn begin_revision(&self, event_id: &str, stem: &str) {
        self.ack(event_id, "Awaiting your revision note...").await;
        let prompt = format!("Reply to this message with the changes you want for `{stem}`.");
        match self.channel.request_reply(&prompt).await {
            Ok(prompt_id) => {
                self.pending_revisions
                    .lock()
                    .await
                    .insert(prompt_id, stem.to_string());
            }
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: failed to send revision prompt");
            }
        }
    }

    /// Revision replies update proposals; any other free-form text runs a Liberado chat turn when
    /// a surface is attached.
    async fn handle_message(&self, text: &str, reply_to_prompt: Option<String>) {
        // The human just messaged us — stamp the shared activity clock so a pending cron brief holds
        // off until this conversation goes quiet.
        if let Some(clock) = &self.last_activity {
            *clock.lock().await = Some(std::time::Instant::now());
        }

        // Revision path: reply to one of our request_reply prompts.
        if let Some(prompt_id) = reply_to_prompt {
            let stem = { self.pending_revisions.lock().await.remove(&prompt_id) };
            if let Some(stem) = stem {
                let note = text.trim();
                if note.is_empty() {
                    self.send_text("Revision note was empty — please try again.")
                        .await;
                    return;
                }
                let _ = self.channel.set_typing().await;
                let pulse = self.spawn_typing_pulse();
                self.apply_revision(&stem, note).await;
                pulse.abort();
                return;
            }
        }

        let text = text.trim();
        if text.is_empty() {
            self.send_text("I only handle text messages for now.").await;
            return;
        }

        let Some(chat) = self.chat.as_ref() else {
            tracing::info!(
                channel = self.channel.name(),
                "free-form message received but no chat surface attached — ignored"
            );
            return;
        };

        if text == "/start" || text == "/help" {
            self.send_text(
                "Liberado is online. Send a normal message to chat. \
                 Proposal Approve/Revise/Reject buttons still work as before.",
            )
            .await;
            return;
        }

        use std::sync::atomic::Ordering;

        // Claim a sequence number and note how much was already running. Both are read back after
        // the turn to decide what the human needs told.
        let seq = self.latest_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let already_running = self.in_flight.fetch_add(1, Ordering::SeqCst);

        tracing::info!(
            channel = self.channel.name(),
            len = text.len(),
            seq,
            already_running,
            "chat message received"
        );

        // Say so when work is genuinely in flight — and only then. Turns now run concurrently, so
        // "I answered you, and something else is still going" is a real state with no outward sign.
        // Silence there would be the bot looking idle while it is not. In ordinary back-and-forth
        // `already_running` is 0 and this never fires.
        if already_running > 0 {
            self.send_text(&concurrency_notice(already_running)).await;
        }

        let _ = self.channel.set_typing().await;
        let pulse = self.spawn_typing_pulse();
        let outcome = chat.reply(text).await;
        pulse.abort();
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        // If newer messages arrived while this ran, the answer is landing out of order — say what
        // it answers. Telegram has native reply-threading but `MessagingChannel` deliberately does
        // not expose it (it is Telegram-specific), so an inline marker is the portable version.
        let stale = self.latest_seq.load(Ordering::SeqCst) != seq;
        let label = |body: &str| {
            if stale {
                format!(
                    "↩ re: \"{}\"

{body}",
                    preview(text)
                )
            } else {
                body.to_string()
            }
        };

        match outcome {
            Ok(reply) => {
                let reply = reply.trim();
                if reply.is_empty() {
                    self.send_text(&label("(no reply text)")).await;
                } else {
                    self.send_text(&label(reply)).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, seq, "chat turn failed");
                self.send_text(&label(&format!("Sorry — that turn failed: {e}")))
                    .await;
            }
        }
    }

    /// Ask the shared provider to redraft `stem`'s `rationale`/`proposed_action` per `note`, then
    /// write the result back as a **fresh, re-signed, still-Pending** proposal and send new
    /// buttons. Never auto-approves.
    async fn apply_revision(&self, stem: &str, note: &str) {
        let path = proposal_path(stem);

        let content = match self.vault.read(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal not found for revision");
                self.send_text("Could not find that proposal.").await;
                return;
            }
        };

        let mut proposal = match Proposal::from_note(&content) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(stem, error = %e, "approval-bot: proposal note did not parse");
                self.send_text("Could not parse that proposal.").await;
                return;
            }
        };

        if proposal.status != ProposalStatus::Pending {
            self.send_text(&format!(
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
                    self.send_text(&format!(
                        "Could not apply that revision ({e}) — the proposal is unchanged."
                    ))
                    .await;
                    return;
                }
            };

        // proposed_action is a signed field — any revision must get a fresh signature. status stays
        // Pending: only a subsequent Approve tap (pure code) can ever execute this.
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
            self.send_text("Failed to save the revision — try again.")
                .await;
            return;
        }

        if let Err(e) = self
            .channel
            .send_with_actions(
                &format!(
                    "Revised — please review before approving:\n{}",
                    proposal.rationale
                ),
                &approval_action_rows(stem),
            )
            .await
        {
            tracing::warn!(error = %e, "approval-bot: failed to send revised proposal buttons");
        }
    }
}

/// What the provider is asked to return for a revision: a redrafted rationale plus (possibly
/// edited) proposed action.
#[derive(serde::Deserialize)]
struct ProposalRevision {
    rationale: String,
    proposed_action: ProposedAction,
}

/// Pure — the vault-relative path for a proposal's filename stem.
fn proposal_path(stem: &str) -> String {
    format!("{PROPOSALS_DIR}/{stem}.md")
}

/// Pure — the `CompletionRequest` for a revision call.
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

/// Pure — a loose JSON schema for [`ProposalRevision`].
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

/// Told to the human when a message arrives while other turns are still running.
///
/// A function rather than an inline `format!` so a test can assert the **rendered** string. The
/// first version shipped with 18 literal spaces mid-sentence — a line-continuation flattened into
/// the literal — and reached Telegram, because nothing anywhere rendered it.
///
/// Phrased as a parenthetical status note, deliberately. The first wording opened with "On it.",
/// which read as an *answer*: the human had asked a question ("did it dispatch?") and instantly
/// received what looked like a reply to it, while the real answer was still minutes away.
fn concurrency_notice(already_running: usize) -> String {
    let (verb, possessive) = if already_running == 1 {
        (" is", "its")
    } else {
        ("s are", "their")
    };
    format!(
        "(Note: {already_running} earlier request{verb} still running — {possessive} reply will \
         come separately, and may land after this one.)"
    )
}

/// A short echo of the message a late reply is answering, for the out-of-order marker.
///
/// Character-based truncation so a multi-byte boundary can't be split, and newlines collapsed so a
/// multi-line request doesn't turn the marker into a wall of quoted text.
fn preview(text: &str) -> String {
    const MAX: usize = 60;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    format!("{}…", flat.chars().take(MAX).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use liberado_common::Capability;
    use liberado_messaging::MessagingError;
    use liberado_provider::{CompletionResponse, MockProvider};
    use tempfile::TempDir;

    /// Silent channel for unit tests that only exercise vault writes.
    struct NullChannel;

    #[async_trait]
    impl MessagingChannel for NullChannel {
        fn name(&self) -> &str {
            "null"
        }
        async fn send_text(&self, _: &str) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn send_with_actions(
            &self,
            _: &str,
            _: &[Vec<liberado_messaging::ActionButton>],
        ) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn request_reply(&self, _: &str) -> Result<String, MessagingError> {
            Ok("prompt-1".into())
        }
        async fn acknowledge(&self, _: &str, _: &str) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
            Ok(vec![])
        }
    }

    /// Records edit_message + send_text calls so tests can assert how a decision was receipted.
    #[derive(Default)]
    struct RecordingChannel {
        edits: std::sync::Mutex<Vec<(String, String)>>,
        sends: std::sync::Mutex<Vec<String>>,
        acks: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl MessagingChannel for RecordingChannel {
        fn name(&self) -> &str {
            "recording"
        }
        async fn send_text(&self, text: &str) -> Result<(), MessagingError> {
            self.sends.lock().unwrap().push(text.to_string());
            Ok(())
        }
        async fn send_with_actions(
            &self,
            _: &str,
            _: &[Vec<liberado_messaging::ActionButton>],
        ) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn request_reply(&self, _: &str) -> Result<String, MessagingError> {
            Ok("prompt-1".into())
        }
        async fn acknowledge(&self, event_id: &str, text: &str) -> Result<(), MessagingError> {
            self.acks
                .lock()
                .unwrap()
                .push((event_id.to_string(), text.to_string()));
            Ok(())
        }
        async fn edit_message(&self, message_ref: &str, text: &str) -> Result<(), MessagingError> {
            self.edits
                .lock()
                .unwrap()
                .push((message_ref.to_string(), text.to_string()));
            Ok(())
        }
        async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
            Ok(vec![])
        }
    }

    #[test]
    fn proposal_path_joins_the_stem() {
        assert_eq!(proposal_path("prop-1"), "proposals/prop-1.md");
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

    /// A tap on a notification whose proposal has since been archived is the *normal* case, not a
    /// storage fault: the daemon archives the moment a proposal goes terminal, and the Telegram
    /// message stays on screen. Live on 2026-08-01 this logged a vault I/O error three times and
    /// told the operator "Proposal not found" about something approved seconds earlier.
    #[tokio::test]
    async fn an_archived_proposal_is_reported_as_resolved_not_missing() {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let signer = ProposalSigner::random();
        let bot = test_bot(
            vault.clone(),
            signer,
            Arc::new(MockProvider::with_script(
                "m",
                Vec::<CompletionResponse>::new(),
            )),
        );

        // Nothing anywhere yet: genuinely missing.
        assert_eq!(bot.archived_outcome("prop-1").await, None);

        // Archived where the daemon puts a terminal proposal.
        vault
            .write(
                "proposals/archive/approved/prop-1.md",
                "---
id: prop-1
---
",
                None,
                &liberado_common::WriteProvenance::human(),
            )
            .await
            .unwrap();

        assert_eq!(
            bot.archived_outcome("prop-1").await,
            Some("approved"),
            "an archived proposal must be recognised as resolved, so a stale tap gets a true answer"
        );
    }

    fn test_bot(vault: Vault, signer: ProposalSigner, provider: Arc<dyn Provider>) -> ApprovalBot {
        ApprovalBot::new(
            Arc::new(NullChannel),
            vault,
            signer,
            provider,
            TelegramApprovalsTuning::default(),
        )
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

        bot.set_status("cq-1", None, &stem, ProposalStatus::Approved)
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

        bot.set_status("cq-1", None, &stem, ProposalStatus::Rejected)
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
        bot.set_status("cq-1", None, &stem, ProposalStatus::Approved)
            .await;

        assert_eq!(
            vault.attribute("proposals/prop-1.md").await.unwrap(),
            liberado_vault::Attribution::External
        );
    }

    #[tokio::test]
    async fn a_decision_with_a_message_ref_edits_the_message_to_strip_buttons() {
        // The button-cleanup UX: a tap edits the original message (receipt + no buttons) instead of
        // sending a fresh message and leaving the now-stale buttons live.
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.set_status("cq-1", Some("777"), &stem, ProposalStatus::Approved)
            .await;

        let edits = channel.edits.lock().unwrap();
        assert_eq!(edits.len(), 1, "the tapped message should be edited once");
        assert_eq!(edits[0].0, "777", "edits the message the button was on");
        assert!(
            edits[0].1.contains("Approved"),
            "receipt says what was tapped"
        );
        assert!(
            channel.sends.lock().unwrap().is_empty(),
            "no fresh message when we can edit in place"
        );
    }

    #[tokio::test]
    async fn a_decision_without_a_message_ref_falls_back_to_a_fresh_message() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.set_status("cq-1", None, &stem, ProposalStatus::Rejected)
            .await;

        assert!(channel.edits.lock().unwrap().is_empty());
        let sends = channel.sends.lock().unwrap();
        assert_eq!(sends.len(), 1, "no message ref → send a fresh receipt");
        assert!(sends[0].contains("Rejected"));
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

    /// User-visible strings get asserted after **rendering**, not eyeballed in source. The stray
    /// whitespace that shipped in the first version of this notice was invisible in review and
    /// obvious the moment it hit a phone.
    #[test]
    fn the_concurrency_notice_is_clean_and_reads_as_a_status_line() {
        for n in [1usize, 2, 5] {
            let msg = concurrency_notice(n);
            assert!(
                !msg.contains("  "),
                "double space in user-visible text: {msg:?}"
            );
            assert!(!msg.contains('\n'), "stray newline: {msg:?}");
            // Must not read as an answer to what was just asked: the human may have asked a
            // question, and "On it." replied to it wrongly, instantly, and confusingly.
            assert!(msg.starts_with("(Note:"), "{msg:?}");
        }
        assert!(concurrency_notice(1).contains("1 earlier request is still running"));
        assert!(concurrency_notice(3).contains("3 earlier requests are still running"));
    }

    #[test]
    fn preview_flattens_and_truncates_for_the_out_of_order_marker() {
        assert_eq!(preview("short question"), "short question");
        // Newlines collapse — a multi-line request must not become a wall of quoted text.
        assert_eq!(
            preview(
                "line one
  line two"
            ),
            "line one line two"
        );
        let long =
            "research the current state of the webassembly component model tooling ecosystem";
        let p = preview(long);
        assert!(p.ends_with('…'));
        assert!(p.chars().count() <= 61, "60 chars plus the ellipsis");
        // Multi-byte input must not panic or split a boundary.
        let emoji = "🎉".repeat(100);
        assert!(preview(&emoji).chars().count() <= 61);
    }

    #[tokio::test]
    async fn handle_action_approve_calls_set_status_approved() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("approve", &stem, "evt-1", None).await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }

    #[tokio::test]
    async fn handle_action_reject_calls_set_status_rejected() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("reject", &stem, "evt-2", None).await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    #[tokio::test]
    async fn handle_action_revise_calls_begin_revision() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("revise", &stem, "evt-3", None).await;

        let revisions = bot.pending_revisions.lock().await;
        assert!(
            revisions.values().any(|s| s == &stem),
            "revision for stem should be registered"
        );
    }

    #[tokio::test]
    async fn handle_action_deny_calls_set_permission_scope_none() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) =
            temp_vault_with_permission_request(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("deny", &stem, "evt-4", None).await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    #[tokio::test]
    async fn handle_action_once_calls_set_permission_scope_once() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) =
            temp_vault_with_permission_request(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("once", &stem, "evt-5", None).await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
        assert_eq!(proposal.approved_scope, Some(GrantScope::Once));
    }

    #[tokio::test]
    async fn handle_action_session_calls_set_permission_scope_session() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) =
            temp_vault_with_permission_request(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("session", &stem, "evt-6", None).await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
        assert_eq!(proposal.approved_scope, Some(GrantScope::Session));
    }

    #[tokio::test]
    async fn handle_action_everywhere_calls_set_permission_scope_everywhere() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) =
            temp_vault_with_permission_request(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_action("everywhere", &stem, "evt-7", None).await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
        assert_eq!(proposal.approved_scope, Some(GrantScope::Everywhere));
    }

    #[tokio::test]
    async fn ack_calls_channel_acknowledge() {
        let signer = ProposalSigner::random();
        let (vault, _dir, _stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.ack("evt-8", "testing ack").await;

        let acks = channel.acks.lock().unwrap();
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].1, "testing ack");
    }

    #[tokio::test]
    async fn handle_event_dispatches_actions() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_event(InboundEvent::Action {
            action: "approve".into(),
            correlation_id: stem.clone(),
            event_id: "evt-action".into(),
            message_ref: None,
        })
        .await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }

    #[tokio::test]
    async fn handle_event_ignores_bot_messages() {
        let signer = ProposalSigner::random();
        let (vault, _dir, _stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.handle_event(InboundEvent::Message {
            text: "bot message".into(),
            reply_to_prompt: None,
            from_bot: true,
        })
        .await;

        // No sends — bot messages are filtered out entirely.
        assert!(channel.sends.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn handle_message_revision_reply_is_routed() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer.clone(),
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.begin_revision("evt-rev", &stem).await;

        // The channel sends a revision prompt which gets mapped in pending_revisions.
        // Simulate a message that is a reply to that prompt.
        bot.handle_message("new rationale please", Some("prompt-1".into()))
            .await;

        // After handling, pending_revisions should have been consumed.
        let revisions = bot.pending_revisions.lock().await;
        assert!(
            !revisions.contains_key("prompt-1"),
            "revision prompt should be consumed"
        );
    }

    #[tokio::test]
    async fn handle_message_non_revision_ignores_when_no_chat_surface() {
        let signer = ProposalSigner::random();
        let (vault, _dir, _stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        // No chat surface attached — text messages should be ignored (no send).
        bot.handle_message("hello there", None).await;

        assert!(channel.sends.lock().unwrap().is_empty());
    }

    #[tokio::test]
    /// Empty and whitespace-only input is answered with the "text messages" hint rather than
    /// silently dropped — a blank send should tell the human what the bot accepts, not look dead.
    async fn handle_message_empty_text_replies_with_the_text_only_hint() {
        let signer = ProposalSigner::random();
        let (vault, _dir, _stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.handle_message("", None).await;
        bot.handle_message("   ", None).await;

        let sends = channel.sends.lock().unwrap();
        assert_eq!(sends.len(), 2);
        assert!(sends[0].contains("text messages"));
        assert!(sends[1].contains("text messages"));
    }

    #[tokio::test]
    async fn begin_revision_records_prompt_stem_mapping() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault, signer, Arc::new(MockProvider::new("mock")));

        bot.begin_revision("evt-br", &stem).await;

        let revisions = bot.pending_revisions.lock().await;
        assert_eq!(revisions.get("prompt-1"), Some(&stem));
    }

    #[tokio::test]
    async fn handle_message_slash_commands_when_no_chat_surface() {
        let signer = ProposalSigner::random();
        let (vault, _dir, _stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());

        struct DummyChat;
        #[async_trait]
        impl ChatSurface for DummyChat {
            async fn reply(&self, _: &str) -> Result<String, String> {
                Ok("chat reply".into())
            }
        }

        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        )
        .with_chat(Arc::new(DummyChat));

        bot.handle_message("/start", None).await;

        let sends = channel.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].contains("Liberado is online"));
    }

    #[tokio::test]
    async fn handle_event_with_message_ref_is_forwarded() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let bot = test_bot(vault.clone(), signer, Arc::new(MockProvider::new("mock")));

        bot.handle_event(InboundEvent::Action {
            action: "reject".into(),
            correlation_id: stem.clone(),
            event_id: "evt-with-ref".into(),
            message_ref: Some("msg-42".into()),
        })
        .await;

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Rejected);
    }

    async fn temp_vault_with_permission_request(
        signer: &ProposalSigner,
        status: ProposalStatus,
    ) -> (Vault, TempDir, String) {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        let stem = "prop-1";
        let mut proposal = Proposal::pending(
            stem,
            "corr-1",
            "liberado",
            ProposedAction::External {
                description: "do something privileged".into(),
            },
            "needs permission",
        );
        proposal.requested_grant = Some(Capability::AskHuman);
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
        (vault, dir, stem.to_string())
    }

    #[tokio::test]
    async fn handle_message_help_slash_command_is_recognized() {
        let signer = ProposalSigner::random();
        let (vault, _dir, _stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RecordingChannel::default());

        struct DummyChat;
        #[async_trait]
        impl ChatSurface for DummyChat {
            async fn reply(&self, _: &str) -> Result<String, String> {
                Ok("chat reply".into())
            }
        }

        let bot = ApprovalBot::new(
            channel.clone(),
            vault,
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        )
        .with_chat(Arc::new(DummyChat));

        bot.handle_message("/help", None).await;

        let sends = channel.sends.lock().unwrap();
        assert_eq!(sends.len(), 1);
        assert!(sends[0].contains("Liberado is online"));
    }

    #[tokio::test]
    async fn set_permission_scope_refuses_non_pending_proposal() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) =
            temp_vault_with_permission_request(&signer, ProposalStatus::Approved).await;
        let channel = Arc::new(RecordingChannel::default());
        let bot = ApprovalBot::new(
            channel.clone(),
            vault.clone(),
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );

        bot.handle_action("once", &stem, "evt-np", None).await;

        // The proposal should NOT have been modified — it was already Approved.
        let acks = channel.acks.lock().unwrap();
        assert!(
            acks.iter()
                .any(|(_, text)| text.contains("Already decided")),
            "expected already-decided ack, got {:?}",
            *acks
        );
    }

    async fn temp_vault_only() -> (Vault, TempDir) {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        (vault, dir)
    }

    #[tokio::test]
    async fn from_env_respects_env_vars() {
        let old_token = std::env::var("LIBERADO_TELEGRAM_BOT_TOKEN").ok();
        let old_chat_id = std::env::var("LIBERADO_TELEGRAM_CHAT_ID").ok();

        // Unset → None
        unsafe {
            std::env::remove_var("LIBERADO_TELEGRAM_BOT_TOKEN");
            std::env::remove_var("LIBERADO_TELEGRAM_CHAT_ID");
        }
        let result = ApprovalBot::from_env(
            temp_vault_only().await.0,
            ProposalSigner::random(),
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );
        assert!(result.is_none(), "None when env vars unset");

        // Set → Some
        unsafe {
            std::env::set_var("LIBERADO_TELEGRAM_BOT_TOKEN", "dummy_token");
            std::env::set_var("LIBERADO_TELEGRAM_CHAT_ID", "dummy_chat");
        }
        let result = ApprovalBot::from_env(
            temp_vault_only().await.0,
            ProposalSigner::random(),
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        );
        assert!(result.is_some(), "Some when env vars set");

        // Restore original state
        unsafe {
            match old_token {
                Some(v) => std::env::set_var("LIBERADO_TELEGRAM_BOT_TOKEN", v),
                None => std::env::remove_var("LIBERADO_TELEGRAM_BOT_TOKEN"),
            };
            match old_chat_id {
                Some(v) => std::env::set_var("LIBERADO_TELEGRAM_CHAT_ID", v),
                None => std::env::remove_var("LIBERADO_TELEGRAM_CHAT_ID"),
            };
        }
    }

    struct RunTrackingChannel {
        registered: std::sync::Mutex<Vec<Vec<(String, String)>>>,
        receive_events: std::sync::Mutex<Vec<Vec<InboundEvent>>>,
    }

    impl RunTrackingChannel {
        fn new() -> Self {
            Self {
                registered: std::sync::Mutex::new(Vec::new()),
                receive_events: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn push_receive(&self, events: Vec<InboundEvent>) {
            self.receive_events.lock().unwrap().push(events);
        }
    }

    #[async_trait]
    impl MessagingChannel for RunTrackingChannel {
        fn name(&self) -> &str {
            "run-tracking"
        }
        async fn send_text(&self, _: &str) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn send_with_actions(
            &self,
            _: &str,
            _: &[Vec<liberado_messaging::ActionButton>],
        ) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn request_reply(&self, _: &str) -> Result<String, MessagingError> {
            Ok("prompt-1".into())
        }
        async fn acknowledge(&self, _: &str, _: &str) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn edit_message(&self, _: &str, _: &str) -> Result<(), MessagingError> {
            Ok(())
        }
        async fn register_commands(
            &self,
            commands: &[(String, String)],
        ) -> Result<(), MessagingError> {
            self.registered.lock().unwrap().push(commands.to_vec());
            Ok(())
        }
        async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
            let batch = {
                let mut q = self.receive_events.lock().unwrap();
                if q.is_empty() {
                    None
                } else {
                    Some(q.drain(..).collect::<Vec<_>>().concat())
                }
            };
            match batch {
                Some(events) => Ok(events),
                None => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        }
    }

    #[tokio::test]
    async fn run_registers_commands_and_processes_events() {
        let signer = ProposalSigner::random();
        let (vault, _dir, stem) = temp_vault_with_proposal(&signer, ProposalStatus::Pending).await;
        let channel = Arc::new(RunTrackingChannel::new());
        channel.push_receive(vec![InboundEvent::Action {
            action: "approve".into(),
            correlation_id: stem.clone(),
            event_id: "run-ev".into(),
            message_ref: None,
        }]);

        let bot = ApprovalBot::new(
            channel.clone(),
            vault.clone(),
            signer,
            Arc::new(MockProvider::new("mock")),
            TelegramApprovalsTuning::default(),
        )
        .with_command_menu(vec![("start".into(), "Start".into())]);

        let handle = tokio::spawn(async move { bot.run().await });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        handle.abort();

        {
            let registered = channel.registered.lock().unwrap();
            assert!(
                !registered.is_empty(),
                "register_commands should have been called"
            );
            assert_eq!(registered[0][0].0, "start");
        }

        let content = vault.read("proposals/prop-1.md").await.unwrap();
        let proposal = Proposal::from_note(&content).unwrap();
        assert_eq!(proposal.status, ProposalStatus::Approved);
    }
}
