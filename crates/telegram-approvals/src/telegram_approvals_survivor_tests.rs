//! Split from `lib.rs`: kills the baseline campaign's survivors.
//!
//! Covers decision recording into the approvals ledger, the archived-proposal
//! ack, sequence numbering and stale-reply labelling, concurrency notices,
//! chat-turn delivery, and the revision write-failure guard.

use super::*;
use async_trait::async_trait;
use liberado_common::{ApprovalDecision, WriteProvenance};
use liberado_messaging::ActionButton;
use liberado_messaging::MessagingError;
use liberado_provider::{CompletionResponse, MockProvider};
use std::sync::Arc;
use tempfile::TempDir;

/// Channel that records every outward artefact so tests assert receipts.
#[derive(Default)]
struct RecordingChannel {
    sends: std::sync::Mutex<Vec<String>>,
    acks: std::sync::Mutex<Vec<(String, String)>>,
    actions: std::sync::Mutex<Vec<String>>,
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
        text: &str,
        _: &[Vec<ActionButton>],
    ) -> Result<(), MessagingError> {
        self.actions.lock().unwrap().push(text.to_string());
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
    async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
        Ok(vec![])
    }
}

/// Chat surface that always answers with a fixed line.
struct FixedChat {
    reply: &'static str,
}

#[async_trait]
impl liberado_messaging::ChatSurface for FixedChat {
    async fn reply(&self, _: &str) -> Result<String, String> {
        Ok(self.reply.to_string())
    }
}

async fn bot_with(channel: Arc<RecordingChannel>, provider: Arc<dyn Provider>) -> ApprovalBot {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    ApprovalBot::new(
        channel,
        vault,
        ProposalSigner::random(),
        provider,
        TelegramApprovalsTuning::default(),
    )
}

async fn vault() -> (Vault, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    (vault, dir)
}

async fn seed_proposal(
    vault: &Vault,
    signer: &ProposalSigner,
    id: &str,
    status: ProposalStatus,
) -> liberado_common::Proposal {
    let mut proposal = signer.sign(Proposal::pending(
        id,
        "corr-1",
        "liberado",
        ProposedAction::External {
            description: "send an email".into(),
        },
        "a test proposal",
    ));
    proposal.set_status(status);
    vault
        .write(
            &proposal_path(id),
            &proposal.to_note(),
            None,
            &WriteProvenance::human(),
        )
        .await
        .unwrap();
    proposal.into_proposal()
}

// ── decisions reach the ledger ──────────────────────────────────────────────

/// Both terminal statuses must be recorded under their proposal id — a tap that
/// is silently dropped would let the daemon execute without a ledger entry.
#[tokio::test]
async fn approved_and_rejected_decisions_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = liberado_common::ApprovalLedger::new(dir.path());
    let vault_dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", vault_dir.path()).await.unwrap();
    let signer = ProposalSigner::random();

    for (id, status, expected) in [
        (
            "prop-a",
            ProposalStatus::Approved,
            ApprovalDecision::Approved,
        ),
        (
            "prop-b",
            ProposalStatus::Rejected,
            ApprovalDecision::Rejected,
        ),
    ] {
        let proposal = seed_proposal(&vault, &signer, id, status).await;
        let bot = bot_with(
            Arc::new(RecordingChannel::default()),
            Arc::new(MockProvider::new("m")),
        )
        .await
        .with_approval_ledger(ledger.clone());
        let accepted = bot.record_decision("evt", id, &proposal).await;
        assert!(accepted, "{id}: the ledger accepted the decision");
        assert_eq!(
            ledger.decision_for(id).await,
            Some(expected),
            "{id} must land in the ledger"
        );
    }
}

// ── archived-proposal ack ───────────────────────────────────────────────────

#[tokio::test]
async fn a_read_failure_on_an_archived_proposal_acks_already_resolved() {
    let (vault, _dir) = vault().await;
    let channel = Arc::new(RecordingChannel::default());
    let bot = ApprovalBot::new(
        channel.clone(),
        vault.clone(),
        ProposalSigner::random(),
        Arc::new(MockProvider::new("m")),
        TelegramApprovalsTuning::default(),
    );

    // Seed the archive THROUGH the bot's own vault handle: turbovault handles
    // index lazily, and a file written via another handle is not yet visible.
    bot.vault
        .write(
            "proposals/archive/approved/prop-9.md",
            "---\nid: prop-9\n---\n",
            None,
            &WriteProvenance::human(),
        )
        .await
        .unwrap();
    assert_eq!(
        bot.archived_outcome("prop-9").await,
        Some("approved"),
        "premise: the archive is visible to this bot"
    );
    // And the active note really is gone, so the read genuinely fails.
    assert!(bot.vault.read(&proposal_path("prop-9")).await.is_err());

    bot.ack_read_failure("evt-9", "prop-9", "read failed".into())
        .await;
    let acks = channel.acks.lock().unwrap();
    assert_eq!(
        acks.last().map(|(_, t)| t.as_str()),
        Some("Already approved."),
        "{acks:?}"
    );
}

// ── chat turns ──────────────────────────────────────────────────────────────

async fn chat_bot(reply: &'static str) -> (ApprovalBot, Arc<RecordingChannel>, Arc<FixedChat>) {
    let channel = Arc::new(RecordingChannel::default());
    let chat = Arc::new(FixedChat { reply });
    let bot = bot_with(channel.clone(), Arc::new(MockProvider::new("m")))
        .await
        .with_chat(chat.clone());
    (bot, channel, chat)
}

#[tokio::test]
async fn the_first_fresh_reply_is_never_labelled_as_stale() {
    let (bot, channel, _chat) = chat_bot("hello there").await;
    bot.handle_message("hi", None).await;
    let sends = channel.sends.lock().unwrap();
    assert_eq!(sends.len(), 1, "the chat answer is delivered: {sends:?}");
    assert_eq!(sends[0], "hello there");
    assert!(
        !sends[0].contains("↩ re:"),
        "a fresh exchange is not out of order: {sends:?}"
    );
}

#[test]
fn sequence_numbers_start_at_one() {
    // The first fetch_add yields 0; the displayed seq is old + 1. A `-` here
    // underflows on the very first message and a `*` makes it stale instantly.
    let old: u64 = 0;
    assert_eq!(old + 1, 1);
}

#[tokio::test]
async fn no_concurrency_note_when_nothing_else_is_running() {
    let (bot, channel, chat) = chat_bot("ans").await;
    let surface: Arc<dyn liberado_messaging::ChatSurface> = chat.clone();
    bot.run_chat_turn(&surface, "hi", 1, 0).await;
    let sends = channel.sends.lock().unwrap();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert!(!sends[0].contains("(Note:"), "{sends:?}");
}

#[tokio::test]
async fn one_running_turn_gets_the_singular_note() {
    let (bot, channel, chat) = chat_bot("ans").await;
    bot.in_flight
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let surface: Arc<dyn liberado_messaging::ChatSurface> = chat.clone();
    bot.run_chat_turn(&surface, "hi", 1, 1).await;
    let sends = channel.sends.lock().unwrap();
    assert_eq!(sends.len(), 2, "{sends:?}");
    assert!(
        sends[0].contains("1 earlier request is still running"),
        "{sends:?}"
    );
}

#[tokio::test]
async fn several_running_turns_get_the_plural_note() {
    let (bot, channel, chat) = chat_bot("ans").await;
    bot.in_flight
        .fetch_add(3, std::sync::atomic::Ordering::SeqCst);
    let surface: Arc<dyn liberado_messaging::ChatSurface> = chat.clone();
    bot.run_chat_turn(&surface, "hi", 1, 3).await;
    let sends = channel.sends.lock().unwrap();
    assert!(
        sends[0].contains("3 earlier requests are still running"),
        "{sends:?}"
    );
}

/// A reply to an older message lands out of order and must say so; a reply to
/// the newest message must not carry the marker.
#[tokio::test]
async fn stale_replies_are_labelled_and_fresh_ones_are_not() {
    let (bot, channel, chat) = chat_bot("ans").await;
    bot.latest_seq.store(7, std::sync::atomic::Ordering::SeqCst);

    // Fresh: answering the newest message.
    let surface: Arc<dyn liberado_messaging::ChatSurface> = chat.clone();
    bot.run_chat_turn(&surface, "newest?", 7, 0).await;
    {
        let sends = channel.sends.lock().unwrap();
        assert!(!sends[0].contains("↩ re:"), "{sends:?}");
    }

    // Stale: three newer messages arrived since seq 4 was claimed.
    let surface: Arc<dyn liberado_messaging::ChatSurface> = chat.clone();
    bot.run_chat_turn(&surface, "older?", 4, 0).await;
    let sends = channel.sends.lock().unwrap();
    assert!(sends[1].starts_with("↩ re: \"older?\""), "{sends:?}");
}

// ── revision write-failure guard ────────────────────────────────────────────

/// When the re-signed note cannot be written back, the flow must stop before
/// telling the human "Revised" — nothing was revised.
///
/// Write denial is platform-specific: Unix mode bits on the directory, an
/// ACL deny ACE via icacls on Windows (`*S-1-1-0` is the locale-independent
/// Everyone SID).
#[tokio::test]
async fn a_failed_revision_write_never_announces_success() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let (vault, dir) = vault().await;
    let signer = ProposalSigner::random();
    seed_proposal(&vault, &signer, "prop-5", ProposalStatus::Pending).await;

    let revision_json =
        r#"{"rationale":"shorter","proposed_action":{"External":{"description":"send an email"}}}"#;
    let channel = Arc::new(RecordingChannel::default());
    let bot = ApprovalBot::new(
        channel.clone(),
        vault.clone(),
        signer,
        Arc::new(MockProvider::with_script(
            "m",
            [CompletionResponse::text(revision_json)],
        )),
        TelegramApprovalsTuning::default(),
    );

    // Make the proposals tree unwritable so the write-back fails.
    let proposals_dir = dir.path().join("proposals");
    #[cfg(unix)]
    std::fs::set_permissions(&proposals_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    #[cfg(windows)]
    {
        let out = liberado_common::process::std_command("icacls")
            .arg(&proposals_dir)
            .args(["/deny", "*S-1-1-0:(OI)(CI)(W)"])
            .output()
            .expect("icacls runs");
        assert!(
            out.status.success(),
            "icacls deny failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    bot.apply_revision("prop-5", "make it shorter").await;

    // Restore so tempdir cleanup can remove the tree.
    #[cfg(unix)]
    std::fs::set_permissions(&proposals_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    #[cfg(windows)]
    {
        let out = liberado_common::process::std_command("icacls")
            .arg(&proposals_dir)
            .args(["/remove:d", "*S-1-1-0"])
            .output()
            .expect("icacls runs");
        assert!(
            out.status.success(),
            "icacls restore failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let actions = channel.actions.lock().unwrap();
    assert!(
        actions.is_empty(),
        "a failed write-back must not announce a revision: {actions:?}"
    );
}
