//! Tell the surface that showed a card when a different surface records the decision.

use std::sync::Arc;

use liberado_common::{DecisionVia, Proposal};
use liberado_messaging::{ResolvedElsewhere, permission_receipt, resolved_elsewhere_target};

use super::{ElsewhereNotice, PermissionResolver, ResolveOutcome};

impl PermissionResolver {
    /// Register a surface that can edit a card it already showed.
    pub fn add_resolved_elsewhere(&self, listener: Arc<dyn ResolvedElsewhere>) {
        self.listeners
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(listener);
    }

    /// Channel that receives background cards, when one is configured (`telegram`).
    pub fn set_push_surface(&self, surface: impl Into<String>) {
        *self
            .push_surface
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(surface.into());
    }

    pub(super) fn push_surface_name(&self) -> Option<String> {
        self.push_surface
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

pub(super) fn kept(outcome: ResolveOutcome) -> super::Resolved {
    (outcome, None)
}

pub(super) fn elsewhere_notice(
    proposal: &Proposal,
    via: DecisionVia,
    outcome: &ResolveOutcome,
    push_surface: Option<&str>,
) -> Option<ElsewhereNotice> {
    let ResolveOutcome::Decided {
        emoji,
        label,
        rationale,
        ..
    } = outcome
    else {
        return None;
    };
    let surface = resolved_elsewhere_target(
        proposal.origin.map(|origin| origin.as_str()),
        via.as_str(),
        push_surface,
    )?;
    Some(ElsewhereNotice {
        surface,
        proposal_id: proposal.id.clone(),
        receipt: permission_receipt(emoji, label, rationale),
    })
}

pub(super) async fn fan_out(
    listeners: &std::sync::Mutex<Vec<Arc<dyn ResolvedElsewhere>>>,
    notice: ElsewhereNotice,
) {
    let hooks = listeners
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    for hook in hooks {
        if hook.surface_id() == notice.surface {
            hook.on_resolved_elsewhere(&notice.proposal_id, &notice.receipt)
                .await;
        }
    }
}
