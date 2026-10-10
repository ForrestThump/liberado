//! The Telegram callback and `PermissionResolver::resolve` share one `Arc`.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_common::{
    ApprovalLedger, Capability, DecisionVia, Proposal, ProposalSigner, ProposedAction,
    WriteProvenance,
};
use liberado_config_loader::TelegramApprovalsTuning;
use liberado_messaging::{ActionButton, InboundEvent, MessagingChannel, MessagingError};
use liberado_provider::MockProvider;
use liberado_vault::Vault;

use crate::{ApprovalBot, PermissionResolver, ResolveOutcome};

struct Quiet;

#[async_trait]
impl MessagingChannel for Quiet {
    fn name(&self) -> &str {
        "quiet"
    }
    async fn send_text(&self, _: &str) -> Result<(), MessagingError> {
        Ok(())
    }
    async fn send_with_actions(
        &self,
        _: &str,
        _: &[Vec<ActionButton>],
    ) -> Result<(), MessagingError> {
        Ok(())
    }
    async fn request_reply(&self, _: &str) -> Result<String, MessagingError> {
        Ok("prompt".into())
    }
    async fn acknowledge(&self, _: &str, _: &str) -> Result<(), MessagingError> {
        Ok(())
    }
    async fn receive(&self, _: &mut String) -> Result<Vec<InboundEvent>, MessagingError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn telegram_callback_and_direct_resolve_share_one_resolver() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    let signer = ProposalSigner::random();
    let mut proposal = Proposal::pending(
        "perm-share",
        "corr-1",
        "liberado",
        ProposedAction::External {
            description: "do something privileged".into(),
        },
        "needs permission",
    );
    proposal.requested_grant = Some(Capability::AskHuman);
    let signed = signer.sign(proposal);
    vault
        .write(
            "proposals/perm-share.md",
            &signed.to_note(),
            None,
            &WriteProvenance::human(),
        )
        .await
        .unwrap();

    let ledger = ApprovalLedger::new(dir.path());
    let resolver = Arc::new(PermissionResolver::new(vault.clone(), Some(ledger.clone())));
    let bot = ApprovalBot::new(
        Arc::new(Quiet),
        vault,
        signer,
        Arc::new(MockProvider::new("mock")),
        TelegramApprovalsTuning::default(),
    )
    .with_resolver(Arc::clone(&resolver));
    assert!(Arc::ptr_eq(&bot.resolver, &resolver));

    bot.handle_action("once", "perm-share", "evt-share", None)
        .await;
    let second = resolver
        .resolve("perm-share", "deny", DecisionVia::Webui)
        .await;
    assert_eq!(
        second,
        ResolveOutcome::AlreadyDecided {
            action: "once",
            label: "Approved once".into(),
        }
    );
    let lines = std::fs::read_to_string(ledger.path()).unwrap();
    assert_eq!(lines.lines().count(), 1);
    assert!(lines.contains("\"by\":\"telegram\""));
}
