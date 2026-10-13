//! Chat-turn approval stamp, plus the risk-gate switch that sits next to it.
//!
//! Both live here so `sessions.rs` stays inside its ploc ceiling. The gate
//! builder lives here for the same reason: the surface and the gate must share
//! one session grant, and a second copy of that construction is how they drifted.

use std::sync::{Arc, Mutex};

use liberado_common::{
    ApprovalOrigin, CapabilitySet, DEFAULT_POOL, PermissionRoute, route_permission,
};
use liberado_conversation_store::Ulid;
use liberado_executor::{ApprovalStamp, PermissionSink, RiskGatedToolRuntime, ToolRuntime};

use super::ChatSessions;

/// Sticky Telegram id and the scope-button sender. The server fills both after `Arc::new`.
pub(super) struct ApprovalHooks {
    sticky_slot: Arc<Mutex<Option<Ulid>>>,
    permission_sink: Mutex<Option<Arc<dyn PermissionSink>>>,
}

impl ApprovalHooks {
    pub(super) fn new() -> Self {
        Self {
            sticky_slot: Arc::new(Mutex::new(None)),
            permission_sink: Mutex::new(None),
        }
    }
}

impl ChatSessions {
    /// The slot the server fills with the sticky Telegram session id.
    pub fn sticky_slot(&self) -> Arc<Mutex<Option<Ulid>>> {
        Arc::clone(&self.approval_hooks.sticky_slot)
    }

    /// Attach the Telegram button sender. Interior mutability so this can run after `Arc::new`.
    pub fn set_permission_sink(&self, sink: Arc<dyn PermissionSink>) {
        *self
            .approval_hooks
            .permission_sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
    }

    /// Whether a Telegram button sender is attached.
    pub fn has_permission_sink(&self) -> bool {
        self.approval_hooks
            .permission_sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    fn sticky_label(&self) -> Option<String> {
        self.approval_hooks
            .sticky_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .map(|id| id.to_string())
    }

    /// Stamp session and origin onto a chat risk gate, and attach the Telegram sink when this
    /// session is the sticky one. The grant already chosen by the caller is left alone.
    pub(super) fn stamp_approval(
        &self,
        session: Ulid,
        mut gated: RiskGatedToolRuntime,
    ) -> RiskGatedToolRuntime {
        let session_id = session.to_string();
        let origin =
            match route_permission(Some(&session_id), self.sticky_label().as_deref(), false) {
                PermissionRoute::TelegramAndWeb { .. } => ApprovalOrigin::Telegram,
                PermissionRoute::WebOnly { .. } => ApprovalOrigin::Web,
                PermissionRoute::Background { .. } => ApprovalOrigin::Background,
            };
        gated = gated.with_approval(ApprovalStamp { session_id, origin });
        if origin == ApprovalOrigin::Telegram
            && let Some(sink) = self
                .approval_hooks
                .permission_sink
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        {
            gated = gated.with_permission_sink(sink);
        }
        gated
    }

    /// Wrap `inner` in the chat risk gate and stamp this session on it.
    ///
    /// `grant` is the session's authority. Callers pass the set that scoped `inner`,
    /// or the full session grant when dispatcher narrowing has already shrunk the
    /// visible surface. The process-wide grant is the wrong value: the model would
    /// see a session-only tool and this gate would refuse the call.
    ///
    /// Chat is not one of the daemon's named pools. Proposals are tagged
    /// [`DEFAULT_POOL`] so an approved chat proposal runs on the daemon's default
    /// pool orchestrator.
    pub(super) fn gate_with_session_grant(
        &self,
        inner: Arc<dyn ToolRuntime>,
        grant: CapabilitySet,
        user: &str,
        session: Ulid,
    ) -> RiskGatedToolRuntime {
        let mut gated = RiskGatedToolRuntime::new(
            inner,
            grant,
            self.consequences.clone(),
            self.zone_catalog.clone(),
            self.zone_write_classes.clone(),
            self.proposals_dir.clone(),
            user.to_string(),
            session.to_string(),
            self.signer.clone(),
            DEFAULT_POOL,
        )
        .with_risk_waivers(self.risk_waivers.clone());
        if let Some(catalog) = &self.live_catalog {
            gated = gated.with_live_catalog(catalog.clone());
        }
        self.stamp_approval(session, gated)
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
