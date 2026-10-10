//! Channel-neutral permission approvals for the HTTP surface.
//!
//! The resolver is the same object the Telegram bot calls. This module projects its
//! proposals into the wire card the WebUI renders, and sends Telegram buttons for a
//! request raised in the sticky chat.

use std::sync::Arc;

use async_trait::async_trait;
use chat_client_contract::{ApprovalCard, ApprovalOption};
use chrono::Utc;
use liberado_common::{Proposal, recorded_action};
use liberado_executor::PermissionSink;
use liberado_messaging::{decided_phrase, permission_card_options};
use liberado_notify::Notifier;
use liberado_telegram_approvals::PermissionResolver;

use crate::sticky::StickySession;

/// Resolver plus the sticky session the card list joins background requests onto.
pub struct ApprovalHub {
    pub resolver: Arc<PermissionResolver>,
    pub sticky: StickySession,
}

impl ApprovalHub {
    pub fn new(resolver: Arc<PermissionResolver>, sticky: StickySession) -> Self {
        Self { resolver, sticky }
    }

    /// Cards for one chat, oldest first. Empty when nothing was raised there.
    pub async fn cards(&self, session_id: &str) -> Vec<ApprovalCard> {
        let sticky = self.sticky.get().await.map(|id| id.to_string());
        self.resolver
            .list_for_session(session_id, sticky.as_deref())
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

fn card_from_proposal(proposal: &Proposal) -> ApprovalCard {
    let action = recorded_action(proposal, Utc::now());
    let pending = action == "pending";
    ApprovalCard {
        id: proposal.id.clone(),
        title: "Permission".into(),
        summary: proposal.rationale.clone(),
        options: permission_card_options()
            .into_iter()
            .map(|option| ApprovalOption {
                action: option.action.to_string(),
                label: option.label.to_string(),
            })
            .collect(),
        pending,
        decision: (!pending).then(|| action.to_string()),
        decision_label: (!pending).then(|| decided_phrase(action).to_string()),
        created_at: proposal.created.to_rfc3339(),
    }
}

/// Telegram scope buttons for a chat that is the sticky session.
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
