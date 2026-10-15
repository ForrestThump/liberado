//! A Matrix decision is recorded as `matrix`, and `/help` reaches the chat surface.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use liberado_common::{
    Capability, ChannelKind, DecisionVia, GrantScope, Proposal, ProposalSigner, ProposalStatus,
    ProposedAction, WriteProvenance,
};
use liberado_config_loader::TelegramApprovalsTuning;
use liberado_messaging::{ChatSurface, InboundEvent, MessagingChannel, MessagingError};
use liberado_provider::{MockProvider, Provider};
use liberado_vault::Vault;

use crate::ApprovalBot;

struct QuietChannel {
    sent: Mutex<Vec<String>>,
    defer_help: bool,
}

#[async_trait]
impl MessagingChannel for QuietChannel {
    fn name(&self) -> &str {
        "Matrix"
    }

    fn defer_help_to_chat(&self) -> bool {
        self.defer_help
    }

    async fn send_text(&self, text: &str) -> Result<(), MessagingError> {
        self.sent.lock().expect("sent").push(text.to_string());
        Ok(())
    }

    async fn send_with_actions(
        &self,
        text: &str,
        _: &[Vec<liberado_messaging::ActionButton>],
    ) -> Result<(), MessagingError> {
        self.send_text(text).await
    }

    async fn request_reply(&self, prompt: &str) -> Result<String, MessagingError> {
        self.send_text(prompt).await?;
        Ok("prompt".into())
    }

    async fn acknowledge(&self, _: &str, _: &str) -> Result<(), MessagingError> {
        Ok(())
    }

    async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
        Ok(Vec::new())
    }
}

struct ListingChat;

#[async_trait]
impl ChatSurface for ListingChat {
    async fn reply(&self, text: &str) -> Result<String, String> {
        Ok(format!("listed {text}"))
    }
}

async fn permission_vault(signer: &ProposalSigner) -> (Vault, tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
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
    proposal.set_status(ProposalStatus::Pending);
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

fn bot(channel: Arc<QuietChannel>, vault: Vault, signer: ProposalSigner) -> ApprovalBot {
    ApprovalBot::new(
        channel,
        vault,
        signer,
        Arc::new(MockProvider::new("mock")) as Arc<dyn Provider>,
        TelegramApprovalsTuning::default(),
    )
}

#[tokio::test]
async fn a_matrix_permission_decision_is_recorded_as_matrix() {
    let signer = ProposalSigner::random();
    let (vault, _dir, stem) = permission_vault(&signer).await;
    let channel = Arc::new(QuietChannel {
        sent: Mutex::new(Vec::new()),
        defer_help: true,
    });
    let bot = bot(channel, vault.clone(), signer).with_decision_channel(ChannelKind::Matrix);
    bot.handle_action("once", &stem, "evt", None).await;
    let note = vault.read("proposals/prop-1.md").await.unwrap();
    let proposal = Proposal::from_note(&note).unwrap();
    assert_eq!(proposal.approved_scope, Some(GrantScope::Once));
    assert_eq!(
        proposal.decided_via,
        Some(DecisionVia::Channel(ChannelKind::Matrix))
    );
    assert!(note.contains("decided_via: matrix"), "{note}");
}

#[tokio::test]
async fn matrix_help_reaches_the_shared_command_list() {
    let signer = ProposalSigner::random();
    let (vault, _dir, _) = permission_vault(&signer).await;
    let channel = Arc::new(QuietChannel {
        sent: Mutex::new(Vec::new()),
        defer_help: true,
    });
    let bot = bot(Arc::clone(&channel), vault, signer)
        .with_decision_channel(ChannelKind::Matrix)
        .with_chat(Arc::new(ListingChat));
    bot.handle_message("/help", None).await;
    let sent = channel.sent.lock().expect("sent").clone();
    assert_eq!(sent, vec!["listed /help".to_string()]);
}
