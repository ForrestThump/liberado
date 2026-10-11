//! Chat-turn approval stamp, plus the risk-gate switch that sits next to it.
//!
//! Both live here so `sessions.rs` stays inside its ploc ceiling.

use std::sync::{Arc, Mutex};

use liberado_common::{
    ApprovalOrigin, ChannelKind, PermissionRoute, SessionChannel, channel_for_session,
    route_permission,
};
use liberado_conversation_store::Ulid;
use liberado_executor::{ApprovalStamp, PermissionSink, RiskGatedToolRuntime};

use super::ChatSessions;

/// Bound channel sessions and the scope-button senders. The server fills both after `Arc::new`.
pub(super) struct ApprovalHooks {
    binding_slot: Arc<Mutex<Vec<SessionChannel>>>,
    permission_sinks: Mutex<Vec<(ChannelKind, Arc<dyn PermissionSink>)>>,
}

impl ApprovalHooks {
    pub(super) fn new() -> Self {
        Self {
            binding_slot: Arc::new(Mutex::new(Vec::new())),
            permission_sinks: Mutex::new(Vec::new()),
        }
    }
}

impl ChatSessions {
    /// The slot the server fills with the channel-binding snapshot.
    pub fn binding_slot(&self) -> Arc<Mutex<Vec<SessionChannel>>> {
        Arc::clone(&self.approval_hooks.binding_slot)
    }

    /// Attach the button sender for `channel`. Interior mutability so this can run after `Arc::new`.
    pub fn set_permission_sink(&self, channel: ChannelKind, sink: Arc<dyn PermissionSink>) {
        let mut sinks = self
            .approval_hooks
            .permission_sinks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sinks.retain(|(kind, _)| *kind != channel);
        sinks.push((channel, sink));
    }

    /// Whether any channel button sender is attached.
    pub fn has_permission_sink(&self) -> bool {
        !self
            .approval_hooks
            .permission_sinks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    fn bound_channel(&self, session_id: &str) -> Option<ChannelKind> {
        let bindings = self
            .approval_hooks
            .binding_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        channel_for_session(&bindings, session_id)
    }

    fn sink_for(&self, channel: ChannelKind) -> Option<Arc<dyn PermissionSink>> {
        self.approval_hooks
            .permission_sinks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(kind, _)| *kind == channel)
            .map(|(_, sink)| Arc::clone(sink))
    }

    /// Stamp session and origin onto a chat risk gate, and attach that channel's sink when this
    /// session is bound to one. The grant already chosen by the caller is left alone.
    pub(super) fn stamp_approval(
        &self,
        session: Ulid,
        mut gated: RiskGatedToolRuntime,
    ) -> RiskGatedToolRuntime {
        let session_id = session.to_string();
        let origin = approval_origin(&session_id, self.bound_channel(&session_id));
        gated = gated.with_approval(ApprovalStamp { session_id, origin });
        if let Some(channel) = origin.channel()
            && let Some(sink) = self.sink_for(channel)
        {
            gated = gated.with_permission_sink(sink);
        }
        gated
    }

    /// Whether runtime risk/zone/consequence gates must wrap tool calls.
    ///
    /// A boot-time empty consequence snapshot is **not** enough to skip gating when a live
    /// catalog is attached — empty→add hot-reload can register write peers after construction.
    pub(super) fn risk_gate_enabled(&self) -> bool {
        self.live_catalog.is_some()
            || !self.consequences.is_empty()
            || !self.zone_catalog.is_empty()
    }
}

fn approval_origin(session_id: &str, bound: Option<ChannelKind>) -> ApprovalOrigin {
    match route_permission(Some(session_id), bound, None, false) {
        PermissionRoute::ChannelAndWeb { channel, .. } => ApprovalOrigin::Channel(channel),
        PermissionRoute::WebOnly { .. } => ApprovalOrigin::Web,
        PermissionRoute::Background { .. } => ApprovalOrigin::Background,
    }
}
