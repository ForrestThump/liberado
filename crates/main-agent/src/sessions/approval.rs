//! Chat-turn approval stamp, plus the risk-gate switch that sits next to it.
//!
//! Both live here so `sessions.rs` stays inside its ploc ceiling. The gate
//! builder lives here for the same reason: the surface and the gate must share
//! one session grant, and a second copy of that construction is how they drifted.

use std::sync::{Arc, Mutex};

use liberado_common::{
    ApprovalOrigin, CapabilitySet, ChannelKind, DEFAULT_POOL, PermissionRoute, SessionChannel,
    channel_for_session, route_permission,
};
use liberado_conversation_store::Ulid;
use liberado_executor::{ApprovalStamp, PermissionSink, RiskGatedToolRuntime, ToolRuntime};

use super::ChatSessions;

/// Resolves a chat profile name → the `policy.toml` grant `component` it borrows its authority
/// from. Wired from the daemon's config at boot; `None` for an unknown or disabled profile.
pub type ProfileComponentResolver = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

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
    /// Attach the profile→component resolver that says whose authority a chat approval widens.
    ///
    /// Without it every chat approval falls back to the daemon's pool component (`dispatcher`),
    /// which is the shared principal every chat can already reach through `delegate`.
    pub fn with_profile_components(mut self, resolver: ProfileComponentResolver) -> Self {
        self.profile_components = Some(resolver);
        self
    }

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

    /// Stamp session, origin and grant component onto a chat risk gate, and attach that channel's
    /// sink when this session is bound to one. The grant already chosen by the caller is left alone.
    ///
    /// `profile` is this turn's profile name, resolved to the `policy.toml` component it borrows
    /// its authority from. That is where an `Everywhere` approval of this chat's request lands.
    pub(super) fn stamp_approval(
        &self,
        session: Ulid,
        profile: Option<&str>,
        mut gated: RiskGatedToolRuntime,
    ) -> RiskGatedToolRuntime {
        let session_id = session.to_string();
        let origin = approval_origin(&session_id, self.bound_channel(&session_id));
        gated = gated.with_approval(ApprovalStamp {
            session_id,
            origin,
            grant_component: self.grant_component_for_profile(profile),
        });
        if let Some(channel) = origin.channel()
            && let Some(sink) = self.sink_for(channel)
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
    /// pool orchestrator. The pool says where the approved call *runs*; `profile`
    /// says whose authority it widens. They are not the same question, and reading
    /// the first as an answer to the second is what sent every chat approval to
    /// `dispatcher`.
    pub(super) fn gate_with_session_grant(
        &self,
        inner: Arc<dyn ToolRuntime>,
        grant: CapabilitySet,
        user: &str,
        session: Ulid,
        profile: Option<&str>,
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
        self.stamp_approval(session, profile, gated)
    }

    /// The `policy.toml` component a chat profile borrows its grant from.
    ///
    /// `None` when no resolver is wired (test fixtures) or the profile is unknown, which leaves
    /// the daemon on its pool-keyed fallback — the behavior before this was carried at all.
    fn grant_component_for_profile(&self, profile: Option<&str>) -> Option<String> {
        let resolver = self.profile_components.as_ref()?;
        resolver(profile?)
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
