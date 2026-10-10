//! One resolver for every human surface.
//!
//! The Telegram callback and the WebUI `POST` both call [`PermissionResolver::resolve`].
//! The first decision wins. A later tap returns the decision already stored and does not
//! append another ledger line. The note under `proposals/` is the pending store. Decided
//! notes stay readable after the daemon archives them.

use std::path::Path;

use chrono::Utc;
use liberado_common::{
    ApprovalDecision, ApprovalLedger, DecisionVia, GrantScope, PROPOSALS_DIR, Proposal,
    ProposalStatus, WriteProvenance, apply_permission_decision,
};
use liberado_messaging::{already_decided_phrase, decided_phrase, permission_choice};
use liberado_vault::Vault;
use tokio::sync::Mutex;

use crate::proposal_path;

/// What [`PermissionResolver::resolve`] did. HTTP and Telegram map these onto their own replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveOutcome {
    /// This call recorded the decision.
    Decided {
        action: &'static str,
        label: String,
        emoji: &'static str,
        rationale: String,
    },
    /// A decision was already on the note. `label` is the phrase, without the "already decided" prefix.
    AlreadyDecided {
        action: &'static str,
        label: String,
    },
    NotFound,
    UnknownAction,
    NotAPermissionRequest,
    /// The note was present but did not parse.
    Unreadable,
    /// The ledger or the note write failed. The caller should ask the human to retry.
    SaveFailed,
}

impl ResolveOutcome {
    /// Body text for a 409: `already decided: Approved once`.
    pub fn already_decided_error(&self) -> Option<String> {
        match self {
            Self::AlreadyDecided { action, .. } => Some(already_decided_phrase(action)),
            _ => None,
        }
    }
}

/// Telegram callback action id for a scope tap. `None` denies.
pub(crate) fn permission_scope_action(scope: Option<GrantScope>) -> &'static str {
    match scope {
        None => "deny",
        Some(GrantScope::Once) => "once",
        Some(GrantScope::Session) => "session",
        Some(GrantScope::Everywhere) => "everywhere",
    }
}

/// Ack text, and the edited-message receipt when this tap recorded the decision.
pub(crate) struct PermissionScopeReply {
    pub ack: Option<String>,
    pub receipt: Option<String>,
}

/// Map a resolve outcome onto the Telegram ack and receipt. Unknown actions warn and send nothing.
pub(crate) fn permission_scope_reply(
    action: &str,
    outcome: &ResolveOutcome,
) -> PermissionScopeReply {
    match outcome {
        ResolveOutcome::Decided {
            label,
            emoji,
            rationale,
            ..
        } => PermissionScopeReply {
            ack: Some(label.clone()),
            receipt: Some(format!("{emoji} {label} — {rationale}")),
        },
        ResolveOutcome::AlreadyDecided { action: stored, .. } => PermissionScopeReply {
            ack: Some(already_decided_phrase(stored)),
            receipt: None,
        },
        ResolveOutcome::NotFound => PermissionScopeReply {
            ack: Some("Request not found.".into()),
            receipt: None,
        },
        ResolveOutcome::NotAPermissionRequest => PermissionScopeReply {
            ack: Some("Not a permission request.".into()),
            receipt: None,
        },
        ResolveOutcome::Unreadable => PermissionScopeReply {
            ack: Some("Could not parse that request.".into()),
            receipt: None,
        },
        ResolveOutcome::UnknownAction => {
            tracing::warn!(action, "unknown permission action");
            PermissionScopeReply {
                ack: None,
                receipt: None,
            }
        }
        ResolveOutcome::SaveFailed => PermissionScopeReply {
            ack: Some("Failed to save — try again.".into()),
            receipt: None,
        },
    }
}

/// Applies a vault-zone permission decision and writes it once.
///
/// `ledger` is optional so unit tests that never attached one keep today's behaviour: the note
/// is still the view the daemon's reactor sees, and a missing ledger skips the append.
pub struct PermissionResolver {
    vault: Vault,
    ledger: Option<ApprovalLedger>,
    lock: Mutex<()>,
}

impl PermissionResolver {
    pub fn new(vault: Vault, ledger: Option<ApprovalLedger>) -> Self {
        Self {
            vault,
            ledger,
            lock: Mutex::new(()),
        }
    }

    /// The ledger this resolver records into, if any. The approval bot uses the same one for
    /// ordinary Approve/Reject taps.
    pub fn ledger(&self) -> Option<ApprovalLedger> {
        self.ledger.clone()
    }

    /// First decision wins. Later calls with a known action return [`ResolveOutcome::AlreadyDecided`].
    pub async fn resolve(&self, stem: &str, action: &str, via: DecisionVia) -> ResolveOutcome {
        let _guard = self.lock.lock().await;
        self.resolve_locked(stem, action, via).await
    }

    /// Permission proposals that belong in `session_id`'s transcript, oldest first.
    ///
    /// Scans the active directory and the archive. A decided card has to stay visible after
    /// the daemon moves the note, including across a restart.
    pub async fn list_for_session(&self, session_id: &str, sticky: Option<&str>) -> Vec<Proposal> {
        let mut found = Vec::new();
        for dir in scan_dirs(self.vault.root()) {
            collect_dir(&dir, session_id, sticky, &mut found).await;
        }
        found.sort_by_key(|proposal| proposal.created);
        found
    }

    async fn resolve_locked(&self, stem: &str, action: &str, via: DecisionVia) -> ResolveOutcome {
        let mut proposal = match self.load(stem).await {
            Loaded::Missing => return ResolveOutcome::NotFound,
            Loaded::Bad => return ResolveOutcome::Unreadable,
            Loaded::Found(proposal) => *proposal,
        };
        let now = Utc::now();
        match apply_permission_decision(&mut proposal, action, via, now) {
            Err(liberado_common::PermissionDecideError::UnknownAction) => {
                ResolveOutcome::UnknownAction
            }
            Err(liberado_common::PermissionDecideError::NotAPermissionRequest) => {
                ResolveOutcome::NotAPermissionRequest
            }
            Ok(liberado_common::PermissionDecide::Already { action }) => already(action),
            Ok(liberado_common::PermissionDecide::Applied { action }) => {
                self.persist(stem, &proposal, via, action).await
            }
        }
    }

    async fn persist(
        &self,
        stem: &str,
        proposal: &Proposal,
        via: DecisionVia,
        action: &'static str,
    ) -> ResolveOutcome {
        if !self.record(proposal, via).await {
            return ResolveOutcome::SaveFailed;
        }
        let path = proposal_path(stem);
        if let Err(error) = self
            .vault
            .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
            .await
        {
            tracing::error!(stem, %error, "permission resolver: failed to write the decision");
            return ResolveOutcome::SaveFailed;
        }
        decided(action, &proposal.rationale)
    }

    async fn record(&self, proposal: &Proposal, via: DecisionVia) -> bool {
        let Some(ledger) = &self.ledger else {
            return true;
        };
        let Some(decision) = ledger_decision(proposal.status) else {
            return true;
        };
        if let Err(error) = ledger.record(&proposal.id, decision, via.as_str()).await {
            tracing::error!(id = %proposal.id, %error, "permission resolver: ledger record failed");
            return false;
        }
        true
    }

    async fn load(&self, stem: &str) -> Loaded {
        match read_note(&self.vault, &proposal_path(stem)).await {
            Loaded::Missing => {}
            other => return other,
        }
        for outcome in ["approved", "rejected", "expired"] {
            let path = format!("{PROPOSALS_DIR}/archive/{outcome}/{stem}.md");
            match read_note(&self.vault, &path).await {
                Loaded::Missing => continue,
                other => return other,
            }
        }
        Loaded::Missing
    }
}

enum Loaded {
    Missing,
    Bad,
    Found(Box<Proposal>),
}

async fn read_note(vault: &Vault, path: &str) -> Loaded {
    let content = match vault.read(path).await {
        Ok(content) => content,
        Err(_) => return Loaded::Missing,
    };
    match Proposal::from_note(&content) {
        Ok(proposal) => Loaded::Found(Box::new(proposal)),
        Err(error) => {
            tracing::warn!(path, %error, "permission resolver: note did not parse");
            Loaded::Bad
        }
    }
}

fn ledger_decision(status: ProposalStatus) -> Option<ApprovalDecision> {
    match status {
        ProposalStatus::Approved | ProposalStatus::Done => Some(ApprovalDecision::Approved),
        ProposalStatus::Rejected => Some(ApprovalDecision::Rejected),
        _ => None,
    }
}

fn already(action: &'static str) -> ResolveOutcome {
    ResolveOutcome::AlreadyDecided {
        label: decided_phrase(action).to_string(),
        action,
    }
}

fn decided(action: &'static str, rationale: &str) -> ResolveOutcome {
    let choice = permission_choice(action);
    ResolveOutcome::Decided {
        action,
        label: decided_phrase(action).to_string(),
        emoji: choice.map(|choice| choice.emoji).unwrap_or("✅"),
        rationale: rationale.to_string(),
    }
}

fn scan_dirs(root: &Path) -> Vec<std::path::PathBuf> {
    let proposals = root.join(PROPOSALS_DIR);
    let archive = proposals.join("archive");
    vec![
        proposals,
        archive.join("approved"),
        archive.join("rejected"),
        archive.join("expired"),
    ]
}

async fn collect_dir(
    dir: &Path,
    session_id: &str,
    sticky: Option<&str>,
    found: &mut Vec<Proposal>,
) {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(_) => return,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        let Ok(proposal) = Proposal::from_note(&content) else {
            continue;
        };
        if liberado_common::card_belongs_to_session(&proposal, session_id, sticky) {
            found.push(proposal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::{
        Capability, GrantScope, ProposalSigner, ProposalStatus, ProposedAction, Zone,
    };
    use tempfile::TempDir;

    fn open_request(id: &str) -> Proposal {
        Proposal::pending(
            id,
            "corr",
            "liberado-chat",
            ProposedAction::ToolCalls(vec![]),
            "needs Write on zone work",
        )
        .with_requested_grant(Capability::Write(Zone::vault("work")))
    }

    async fn vault() -> (Vault, TempDir) {
        let dir = TempDir::new().unwrap();
        let vault = Vault::open("test", dir.path()).await.unwrap();
        (vault, dir)
    }

    async fn write_pending(vault: &Vault, proposal: Proposal) {
        let path = proposal_path(&proposal.id);
        vault
            .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn first_tap_wins_and_a_second_tap_does_not_append_the_ledger() {
        let (vault, dir) = vault().await;
        let ledger = ApprovalLedger::new(dir.path());
        write_pending(&vault, open_request("perm-1")).await;
        let resolver = PermissionResolver::new(vault.clone(), Some(ledger.clone()));

        let first = resolver.resolve("perm-1", "once", DecisionVia::Webui).await;
        let second = resolver
            .resolve("perm-1", "everywhere", DecisionVia::Telegram)
            .await;

        assert!(matches!(
            first,
            ResolveOutcome::Decided { action: "once", .. }
        ));
        assert_eq!(
            second,
            ResolveOutcome::AlreadyDecided {
                action: "once",
                label: "Approved once".into(),
            }
        );
        assert_eq!(
            second.already_decided_error().as_deref(),
            Some("already decided: Approved once")
        );

        let note = Proposal::from_note(&vault.read("proposals/perm-1.md").await.unwrap()).unwrap();
        assert_eq!(note.approved_scope, Some(GrantScope::Once));
        assert_eq!(note.decided_via, Some(DecisionVia::Webui));
        assert_eq!(note.status, ProposalStatus::Approved);

        let lines = tokio::fs::read_to_string(ledger.path()).await.unwrap();
        assert_eq!(lines.lines().count(), 1);
        assert!(lines.contains("\"by\":\"webui\""));
    }

    #[tokio::test]
    async fn a_reloaded_note_stays_decided() {
        let (vault, dir) = vault().await;
        write_pending(&vault, open_request("perm-2")).await;
        let resolver =
            PermissionResolver::new(vault.clone(), Some(ApprovalLedger::new(dir.path())));
        resolver
            .resolve("perm-2", "session", DecisionVia::Telegram)
            .await;

        let again = PermissionResolver::new(vault, None);
        let outcome = again.resolve("perm-2", "deny", DecisionVia::Webui).await;
        assert_eq!(
            outcome,
            ResolveOutcome::AlreadyDecided {
                action: "session",
                label: "Approved for this session".into(),
            }
        );
    }

    #[tokio::test]
    async fn unknown_id_unknown_action_and_ordinary_notes_are_refused() {
        let (vault, _dir) = vault().await;
        let ordinary = Proposal::pending(
            "prop-1",
            "c",
            "s",
            ProposedAction::ToolCalls(vec![]),
            "not a permission",
        );
        write_pending(&vault, ordinary).await;
        let resolver = PermissionResolver::new(vault, None);

        assert_eq!(
            resolver
                .resolve("missing", "once", DecisionVia::Webui)
                .await,
            ResolveOutcome::NotFound
        );
        write_pending(&resolver.vault, open_request("perm-3")).await;
        assert_eq!(
            resolver
                .resolve("perm-3", "revise", DecisionVia::Webui)
                .await,
            ResolveOutcome::UnknownAction
        );
        let reloaded =
            Proposal::from_note(&resolver.vault.read("proposals/perm-3.md").await.unwrap())
                .unwrap();
        assert_eq!(reloaded.status, ProposalStatus::Pending);
        assert_eq!(
            resolver.resolve("prop-1", "once", DecisionVia::Webui).await,
            ResolveOutcome::NotAPermissionRequest
        );
    }

    #[tokio::test]
    async fn cards_follow_the_owning_session_and_background_lands_on_sticky() {
        let (vault, _dir) = vault().await;
        let mut web = open_request("perm-web");
        web.session_id = Some("web".into());
        web.origin = Some(liberado_common::ApprovalOrigin::Web);
        let mut telegram = open_request("perm-tg");
        telegram.session_id = Some("tg".into());
        telegram.origin = Some(liberado_common::ApprovalOrigin::Telegram);
        let mut background = open_request("perm-bg");
        background.origin = Some(liberado_common::ApprovalOrigin::Background);
        for proposal in [web, telegram, background] {
            write_pending(&vault, proposal).await;
        }
        let resolver = PermissionResolver::new(vault, None);

        let web_cards = resolver.list_for_session("web", Some("tg")).await;
        assert_eq!(web_cards.len(), 1);
        assert_eq!(web_cards[0].id, "perm-web");

        let sticky_cards = resolver.list_for_session("tg", Some("tg")).await;
        let mut ids: Vec<_> = sticky_cards.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["perm-bg", "perm-tg"]);
    }

    #[tokio::test]
    async fn an_archived_decision_is_still_already_decided() {
        let (vault, _dir) = vault().await;
        let mut proposal = open_request("perm-arch");
        let signer = ProposalSigner::random();
        apply_permission_decision(&mut proposal, "deny", DecisionVia::Telegram, Utc::now())
            .unwrap();
        let proposal = signer.sign(proposal);
        vault
            .write(
                "proposals/archive/rejected/perm-arch.md",
                &proposal.to_note(),
                None,
                &WriteProvenance::human(),
            )
            .await
            .unwrap();
        let resolver = PermissionResolver::new(vault, None);
        let outcome = resolver
            .resolve("perm-arch", "once", DecisionVia::Webui)
            .await;
        assert_eq!(
            outcome,
            ResolveOutcome::AlreadyDecided {
                action: "deny",
                label: "Denied".into(),
            }
        );
    }

    #[test]
    fn permission_scope_reply_names_every_outcome_and_scope() {
        assert_eq!(permission_scope_action(None), "deny");
        assert_eq!(permission_scope_action(Some(GrantScope::Once)), "once");
        assert_eq!(
            permission_scope_action(Some(GrantScope::Session)),
            "session"
        );
        assert_eq!(
            permission_scope_action(Some(GrantScope::Everywhere)),
            "everywhere"
        );

        let decided = permission_scope_reply(
            "once",
            &ResolveOutcome::Decided {
                action: "once",
                label: "✅ Once".into(),
                emoji: "✅",
                rationale: "needs write".into(),
            },
        );
        assert_eq!(decided.ack.as_deref(), Some("✅ Once"));
        assert_eq!(decided.receipt.as_deref(), Some("✅ ✅ Once — needs write"));

        let cases = [
            (
                ResolveOutcome::AlreadyDecided {
                    action: "session",
                    label: "Approved for this session".into(),
                },
                Some("already decided: Approved for this session"),
                None,
            ),
            (ResolveOutcome::NotFound, Some("Request not found."), None),
            (
                ResolveOutcome::NotAPermissionRequest,
                Some("Not a permission request."),
                None,
            ),
            (
                ResolveOutcome::Unreadable,
                Some("Could not parse that request."),
                None,
            ),
            (ResolveOutcome::UnknownAction, None, None),
            (
                ResolveOutcome::SaveFailed,
                Some("Failed to save — try again."),
                None,
            ),
        ];
        for (outcome, ack, receipt) in cases {
            let reply = permission_scope_reply("nope", &outcome);
            assert_eq!(reply.ack.as_deref(), ack);
            assert_eq!(reply.receipt.as_deref(), receipt);
        }
    }
}
