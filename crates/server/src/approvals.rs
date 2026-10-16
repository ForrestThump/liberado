//! Channel-neutral permission approvals for the HTTP surface.
//!
//! The resolver is the same object the channel bot calls. This module projects its
//! proposals into the wire card the WebUI renders, and sends channel buttons for a
//! request raised in a bound chat.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chat_client_contract::{ApprovalCard, PendingApproval};
use chrono::{DateTime, Utc};
use liberado_common::{
    ApprovalLedger, Capability, ChannelKind, Proposal, ProposedAction, recorded_action,
};
use liberado_executor::PermissionSink;
use liberado_main_agent::ChatSessions;
use liberado_messaging::decided_phrase;
use liberado_notify::Notifier;
use liberado_telegram_approvals::PermissionResolver;
use liberado_vault::Vault;

use crate::bindings::ChannelBindings;

/// Resolver plus the binding registry the card list joins background requests onto.
pub struct ApprovalHub {
    pub resolver: Arc<PermissionResolver>,
    pub bindings: ChannelBindings,
}

impl ApprovalHub {
    pub fn new(resolver: Arc<PermissionResolver>, bindings: ChannelBindings) -> Self {
        Self { resolver, bindings }
    }

    /// Cards for one chat, oldest first. Empty when nothing was raised there.
    pub async fn cards(&self, session_id: &str) -> Vec<ApprovalCard> {
        let bound = self
            .bindings
            .background_session()
            .await
            .map(|id| id.to_string());
        self.resolver
            .list_for_session(session_id, bound.as_deref())
            .await
            .iter()
            .map(card_from_proposal)
            .collect()
    }
}

/// Cards for a conversation. An unwired hub is an empty list, so history stays 200.
pub async fn conversation_cards(hub: &Option<Arc<ApprovalHub>>, id: &str) -> Vec<ApprovalCard> {
    match hub {
        Some(hub) => hub.cards(id).await,
        None => Vec::new(),
    }
}

/// Open permission requests across chats. An unwired hub is an empty list.
pub async fn pending_approval_rows(hub: &Option<Arc<ApprovalHub>>) -> Vec<PendingApproval> {
    let Some(hub) = hub else {
        return Vec::new();
    };
    let now = Utc::now();
    hub.resolver
        .list_pending_permission_requests()
        .await
        .iter()
        .filter_map(|proposal| pending_approval_row(proposal, now))
        .collect()
}

/// One pending permission row. `None` for an ordinary proposal or a decided note.
pub fn pending_approval_row(proposal: &Proposal, now: DateTime<Utc>) -> Option<PendingApproval> {
    if !is_open_permission(proposal, now) {
        return None;
    }
    Some(PendingApproval {
        id: proposal.id.clone(),
        conversation: proposal.session_id.clone(),
        tool: permission_tool(proposal),
        zone: permission_zone(proposal),
        created_at: proposal.created.to_rfc3339(),
    })
}

fn is_open_permission(proposal: &Proposal, now: DateTime<Utc>) -> bool {
    proposal.requested_grant.is_some() && recorded_action(proposal, now) == "pending"
}

fn permission_tool(proposal: &Proposal) -> Option<String> {
    match &proposal.proposed_action {
        ProposedAction::ToolCalls(calls) => calls.first().map(|call| call.tool.clone()),
        _ => None,
    }
}

fn permission_zone(proposal: &Proposal) -> Option<String> {
    match proposal.requested_grant.as_ref() {
        Some(Capability::Write(zone)) => Some(zone.name().to_string()),
        _ => None,
    }
}

fn card_from_proposal(proposal: &Proposal) -> ApprovalCard {
    let action = recorded_action(proposal, Utc::now());
    let pending = action == "pending";
    ApprovalCard {
        id: proposal.id.clone(),
        title: "Permission".into(),
        summary: proposal.rationale.clone(),
        options: crate::schedule_cards::card_options(proposal),
        pending,
        decision: (!pending).then(|| action.to_string()),
        decision_label: (!pending).then(|| decided_phrase(action).to_string()),
        created_at: proposal.created.to_rfc3339(),
    }
}

/// Build the shared resolver and hub, and point chat at the binding snapshot.
///
/// The hub always exists. Background cards join the bound channel chat through it, and HTTP
/// serves cards from it. The sink is attached only when chat and a notifier both exist.
/// `publish_to` still runs for a chat with no notifier, so the risk gate can see the binding.
pub(crate) async fn wire_approvals(
    chat: Option<&Arc<ChatSessions>>,
    bindings: &ChannelBindings,
    vault: Vault,
    ledger_dir: &Path,
    notifier: Option<Arc<dyn Notifier>>,
) -> (Arc<PermissionResolver>, Arc<ApprovalHub>) {
    if let Some(sessions) = chat {
        if let Some(notifier) = notifier {
            sessions.set_permission_sink(
                ChannelKind::Telegram,
                Arc::new(ChannelPermissionSink::new(notifier)),
            );
        }
        bindings.publish_to(sessions.binding_slot()).await;
    }
    let resolver = Arc::new(PermissionResolver::new(
        vault,
        Some(ApprovalLedger::new(ledger_dir)),
    ));
    let hub = Arc::new(ApprovalHub::new(Arc::clone(&resolver), bindings.clone()));
    (resolver, hub)
}

/// The Telegram notifier from the process environment, when both bot variables are set.
pub(crate) fn env_permission_notifier() -> Option<Arc<dyn Notifier>> {
    let notifier = liberado_notify::TelegramNotifier::from_env()?;
    Some(Arc::new(notifier) as Arc<dyn Notifier>)
}

/// Channel scope buttons for a chat that is bound to a channel.
///
/// `liberado-main-agent` cannot depend on `liberado-notify`. The server, which already
/// holds the notifier, implements the sink the risk gate calls.
pub struct ChannelPermissionSink {
    notifier: Arc<dyn Notifier>,
}

impl ChannelPermissionSink {
    pub fn new(notifier: Arc<dyn Notifier>) -> Self {
        Self { notifier }
    }
}

#[async_trait]
impl PermissionSink for ChannelPermissionSink {
    async fn notify_permission(&self, proposal_id: &str, message: &str) -> Result<(), String> {
        self.notifier
            .notify_permission_request(proposal_id, message)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
#[path = "approvals_wire_tests.rs"]
mod wire_tests;

#[cfg(test)]
#[path = "approvals_pending_tests.rs"]
mod pending_tests;
