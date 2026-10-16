//! Schedule-edit cards: Approve applies one edit, Deny writes nothing.
//!
//! These notes are permission requests so the chat card list shows them. The
//! decision is stored as `done` or `rejected` with `approved_scope` left unset.
//! Once, Session, and Everywhere are the same as Approve: one edit, no standing
//! grant, and no write to `grants.overlay.toml`.

use chrono::Utc;
use liberado_common::{DecisionVia, Proposal, ProposalStatus, recorded_action};

use super::{PermissionResolver, ResolveOutcome};

/// Model-facing text when the human taps Deny.
pub const SCHEDULE_DENY_REPLY: &str = "Denied. topology.toml was not changed.";

impl PermissionResolver {
    /// The note, when it parses. Missing and unreadable notes are `None`.
    pub async fn read_proposal(&self, stem: &str) -> Option<Proposal> {
        match self.load(stem).await {
            super::Loaded::Found(proposal) => Some(*proposal),
            _ => None,
        }
    }

    /// Record Approve or Deny for one schedule card.
    ///
    /// `apply` runs only for an open Approve-class tap, and only once. The
    /// returned text is stored as the note rationale and on [`ResolveOutcome::Decided`].
    pub async fn finish_schedule_card<F>(
        &self,
        stem: &str,
        action: &str,
        via: DecisionVia,
        apply: F,
    ) -> ResolveOutcome
    where
        F: FnOnce(&Proposal) -> String,
    {
        let _guard = self.lock.lock().await;
        self.finish_schedule_locked(stem, action, via, apply).await
    }

    async fn finish_schedule_locked<F>(
        &self,
        stem: &str,
        action: &str,
        via: DecisionVia,
        apply: F,
    ) -> ResolveOutcome
    where
        F: FnOnce(&Proposal) -> String,
    {
        let mut proposal = match self.load(stem).await {
            super::Loaded::Missing => return ResolveOutcome::NotFound,
            super::Loaded::Bad => return ResolveOutcome::Unreadable,
            super::Loaded::Found(proposal) => *proposal,
        };
        if proposal.requested_grant.is_none() {
            return ResolveOutcome::NotAPermissionRequest;
        }
        let now = Utc::now();
        if proposal.status != ProposalStatus::Pending || proposal.is_expired_at(now) {
            return super::already(recorded_action(&proposal, now));
        }
        match tap(action) {
            Tap::Unknown => ResolveOutcome::UnknownAction,
            Tap::Deny => {
                proposal.status = ProposalStatus::Rejected;
                proposal.approved_scope = None;
                proposal.decided_at = Some(now);
                proposal.decided_via = Some(via);
                proposal.rationale = SCHEDULE_DENY_REPLY.to_string();
                let (outcome, _) = self.persist(stem, &proposal, via, "deny").await;
                outcome
            }
            Tap::Apply => {
                let reply = apply(&proposal);
                proposal.status = ProposalStatus::Done;
                proposal.approved_scope = None;
                proposal.decided_at = Some(now);
                proposal.decided_via = Some(via);
                proposal.rationale = reply;
                let (outcome, _) = self.persist(stem, &proposal, via, "approved").await;
                outcome
            }
        }
    }
}

enum Tap {
    Apply,
    Deny,
    Unknown,
}

fn tap(action: &str) -> Tap {
    match action {
        "deny" => Tap::Deny,
        "approve" | "once" | "session" | "everywhere" => Tap::Apply,
        _ => Tap::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use liberado_common::{
        Capability, DecisionVia, GrantScope, Proposal, ProposalStatus, ProposedAction,
    };
    use liberado_vault::Vault;

    use super::super::PermissionResolver;
    use super::SCHEDULE_DENY_REPLY;

    fn open_card(id: &str) -> Proposal {
        let mut proposal = Proposal::pending(
            id,
            id,
            "automation-editor",
            ProposedAction::ToolCalls(vec![]),
            "Create schedule 'noon'.",
        )
        .with_requested_grant(Capability::ExecuteTool(
            "automation-editor:create_schedule".into(),
        ));
        proposal.session_id = Some("sess".into());
        proposal.origin = Some(liberado_common::ApprovalOrigin::Web);
        proposal
    }

    async fn write_pending(vault: &Vault, proposal: &Proposal) {
        vault
            .write(
                &crate::proposal_path(&proposal.id),
                &proposal.to_note(),
                None,
                &liberado_common::WriteProvenance::human(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn approve_applies_once_and_does_not_store_a_grant_scope() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        write_pending(&vault, &open_card("sched-1")).await;
        let resolver = PermissionResolver::new(vault.clone(), None);
        let calls = AtomicUsize::new(0);
        let first = resolver
            .finish_schedule_card("sched-1", "session", DecisionVia::Webui, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                "Created schedule 'noon'. The file is on disk.".into()
            })
            .await;
        let second = resolver
            .finish_schedule_card("sched-1", "everywhere", DecisionVia::Webui, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                "again".into()
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        match first {
            super::super::ResolveOutcome::Decided { rationale, .. } => {
                assert!(rationale.contains("on disk"), "{rationale}");
            }
            other => panic!("expected Decided, got {other:?}"),
        }
        assert!(matches!(
            second,
            super::super::ResolveOutcome::AlreadyDecided { .. }
        ));
        let note =
            Proposal::from_note(&vault.read(&crate::proposal_path("sched-1")).await.unwrap())
                .unwrap();
        assert_eq!(note.status, ProposalStatus::Done);
        assert_eq!(note.approved_scope, None);
        assert_ne!(note.approved_scope, Some(GrantScope::Session));
        assert_ne!(note.approved_scope, Some(GrantScope::Everywhere));
    }

    #[tokio::test]
    async fn deny_does_not_call_apply() {
        let dir = tempfile::tempdir().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        write_pending(&vault, &open_card("sched-2")).await;
        let resolver = PermissionResolver::new(vault.clone(), None);
        let calls = AtomicUsize::new(0);
        let outcome = resolver
            .finish_schedule_card("sched-2", "deny", DecisionVia::Webui, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                "should not run".into()
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        match outcome {
            super::super::ResolveOutcome::Decided { rationale, .. } => {
                assert_eq!(rationale, SCHEDULE_DENY_REPLY);
            }
            other => panic!("expected Decided, got {other:?}"),
        }
        let note =
            Proposal::from_note(&vault.read(&crate::proposal_path("sched-2")).await.unwrap())
                .unwrap();
        assert_eq!(note.status, ProposalStatus::Rejected);
        assert_eq!(note.approved_scope, None);
    }
}
