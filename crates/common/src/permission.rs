//! Channel-neutral permission decisions.
//!
//! A vault-zone permission request is still a [`Proposal`] on disk. This module
//! is the decision half that every human surface shares: which chat the request belongs to, and
//! how a tap becomes a status. It does no I/O. The Telegram callback and the WebUI `POST` both
//! call one resolver that applies [`apply_permission_decision`] and then writes the note.

use chrono::{DateTime, Utc};

use crate::proposal::{GrantScope, Proposal, ProposalStatus};

mod channel;

pub use channel::{
    ApprovalOrigin, ChannelKind, DecisionVia, PermissionRoute, SessionChannel, channel_for_session,
    route_permission,
};

/// Whether this permission proposal renders in `session_id`'s transcript.
///
/// A request with a session id belongs to that session only. A request with no session
/// (background, or a note written before sessions were stamped) shows in the bound channel
/// conversation, which is the WebUI view of the chat that already got the message.
pub fn card_belongs_to_session(
    proposal: &Proposal,
    session_id: &str,
    bound_session: Option<&str>,
) -> bool {
    if proposal.requested_grant.is_none() {
        return false;
    }
    if proposal.session_id.as_deref() == Some(session_id) {
        return true;
    }
    if proposal.session_id.is_some() {
        return false;
    }
    bound_session == Some(session_id) && proposal.origin != Some(ApprovalOrigin::Web)
}

/// What applying `action` did. `action` is a template id (`deny`, `once`, `session`, `everywhere`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecide {
    Applied { action: &'static str },
    Already { action: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecideError {
    UnknownAction,
    NotAPermissionRequest,
}

/// First decision wins. A later call returns [`PermissionDecide::Already`] and does not
/// change the note. `action` is `deny` | `once` | `session` | `everywhere`.
pub fn apply_permission_decision(
    proposal: &mut Proposal,
    action: &str,
    via: DecisionVia,
    now: DateTime<Utc>,
) -> Result<PermissionDecide, PermissionDecideError> {
    if proposal.requested_grant.is_none() {
        return Err(PermissionDecideError::NotAPermissionRequest);
    }
    if !action_is_known(action) {
        return Err(PermissionDecideError::UnknownAction);
    }
    if !is_open(proposal, now) {
        return Ok(PermissionDecide::Already {
            action: recorded_action(proposal, now),
        });
    }
    stamp(proposal, action, via, now);
    Ok(PermissionDecide::Applied {
        action: canonical_action(action),
    })
}

fn action_is_known(action: &str) -> bool {
    canonical_action(action) != "unknown"
}

fn canonical_action(action: &str) -> &'static str {
    match action {
        "deny" => "deny",
        "once" => "once",
        "session" => "session",
        "everywhere" => "everywhere",
        _ => "unknown",
    }
}

fn is_open(proposal: &Proposal, now: DateTime<Utc>) -> bool {
    proposal.status == ProposalStatus::Pending && !proposal.is_expired_at(now)
}

fn stamp(proposal: &mut Proposal, action: &str, via: DecisionVia, now: DateTime<Utc>) {
    match action {
        "deny" => {
            proposal.status = ProposalStatus::Rejected;
            proposal.approved_scope = None;
        }
        "once" => approve(proposal, GrantScope::Once),
        "session" => approve(proposal, GrantScope::Session),
        "everywhere" => approve(proposal, GrantScope::Everywhere),
        _ => {}
    }
    proposal.decided_at = Some(now);
    proposal.decided_via = Some(via);
}

fn approve(proposal: &mut Proposal, scope: GrantScope) {
    proposal.status = ProposalStatus::Approved;
    proposal.approved_scope = Some(scope);
}

/// The action id already stored on a decided proposal. `pending` only when it is still open.
pub fn recorded_action(proposal: &Proposal, now: DateTime<Utc>) -> &'static str {
    if proposal.status == ProposalStatus::Pending && proposal.is_expired_at(now) {
        return "expired";
    }
    match proposal.status {
        ProposalStatus::Rejected => "deny",
        ProposalStatus::Expired => "expired",
        ProposalStatus::Approved | ProposalStatus::Done => match proposal.approved_scope {
            Some(GrantScope::Once) => "once",
            Some(GrantScope::Session) => "session",
            Some(GrantScope::Everywhere) => "everywhere",
            None => "approved",
        },
        ProposalStatus::Pending => "pending",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, Zone};
    use crate::proposal::{Proposal, ProposedAction};

    fn open_request() -> Proposal {
        Proposal::pending(
            "perm-1",
            "corr",
            "liberado-chat",
            ProposedAction::ToolCalls(vec![]),
            "needs Write on zone work",
        )
        .with_requested_grant(Capability::Write(Zone::vault("work")))
    }

    #[test]
    fn first_decision_wins_and_a_second_tap_does_not_change_it() {
        let mut proposal = open_request();
        let now = Utc::now();
        let first =
            apply_permission_decision(&mut proposal, "once", DecisionVia::Webui, now).unwrap();
        assert_eq!(first, PermissionDecide::Applied { action: "once" });
        assert_eq!(proposal.status, ProposalStatus::Approved);
        assert_eq!(proposal.approved_scope, Some(GrantScope::Once));
        assert_eq!(proposal.decided_via, Some(DecisionVia::Webui));

        let second = apply_permission_decision(
            &mut proposal,
            "everywhere",
            DecisionVia::Channel(ChannelKind::Telegram),
            now,
        )
        .unwrap();
        assert_eq!(second, PermissionDecide::Already { action: "once" });
        assert_eq!(proposal.approved_scope, Some(GrantScope::Once));
        assert_eq!(proposal.decided_via, Some(DecisionVia::Webui));
    }

    #[test]
    fn deny_rejects_without_a_scope() {
        let mut proposal = open_request();
        apply_permission_decision(
            &mut proposal,
            "deny",
            DecisionVia::Channel(ChannelKind::Telegram),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(proposal.status, ProposalStatus::Rejected);
        assert_eq!(proposal.approved_scope, None);
        assert_eq!(recorded_action(&proposal, Utc::now()), "deny");
    }

    #[test]
    fn unknown_action_and_ordinary_proposal_are_refused() {
        let mut proposal = open_request();
        let err =
            apply_permission_decision(&mut proposal, "revise", DecisionVia::Webui, Utc::now());
        assert_eq!(err, Err(PermissionDecideError::UnknownAction));
        assert_eq!(proposal.status, ProposalStatus::Pending);

        let mut ordinary = Proposal::pending("p", "c", "s", ProposedAction::ToolCalls(vec![]), "r");
        let err = apply_permission_decision(&mut ordinary, "once", DecisionVia::Webui, Utc::now());
        assert_eq!(err, Err(PermissionDecideError::NotAPermissionRequest));
    }

    #[test]
    fn a_decided_note_reloads_and_stays_decided() {
        let mut proposal = open_request();
        proposal.session_id = Some("sess-1".into());
        proposal.origin = Some(ApprovalOrigin::Web);
        apply_permission_decision(&mut proposal, "session", DecisionVia::Webui, Utc::now())
            .unwrap();
        let reloaded = Proposal::from_note(&proposal.to_note()).unwrap();
        assert_eq!(reloaded.session_id.as_deref(), Some("sess-1"));
        assert_eq!(reloaded.origin, Some(ApprovalOrigin::Web));
        assert_eq!(reloaded.decided_via, Some(DecisionVia::Webui));
        assert_eq!(reloaded.approved_scope, Some(GrantScope::Session));
        let again = apply_permission_decision(
            &mut { reloaded.clone() },
            "deny",
            DecisionVia::Channel(ChannelKind::Telegram),
            Utc::now(),
        )
        .unwrap();
        assert_eq!(again, PermissionDecide::Already { action: "session" });
    }

    #[test]
    fn routing_table_matches_the_owning_session() {
        assert_eq!(
            route_permission(Some("tg"), Some(ChannelKind::Telegram), Some("tg"), false),
            PermissionRoute::ChannelAndWeb {
                channel: ChannelKind::Telegram,
                session_id: "tg".into()
            }
        );
        assert_eq!(
            route_permission(Some("mx"), Some(ChannelKind::Matrix), None, false),
            PermissionRoute::ChannelAndWeb {
                channel: ChannelKind::Matrix,
                session_id: "mx".into()
            }
        );
        assert_eq!(
            route_permission(Some("web"), None, Some("tg"), false),
            PermissionRoute::WebOnly {
                session_id: "web".into()
            }
        );
        assert_eq!(
            route_permission(Some("web"), None, None, false),
            PermissionRoute::WebOnly {
                session_id: "web".into()
            }
        );
        assert_eq!(
            route_permission(None, None, Some("tg"), false),
            PermissionRoute::Background {
                bound_session: Some("tg".into())
            }
        );
        assert_eq!(
            route_permission(Some("goal"), Some(ChannelKind::Telegram), Some("tg"), true),
            PermissionRoute::Background {
                bound_session: Some("tg".into())
            }
        );
    }

    #[test]
    fn stored_channel_strings_round_trip() {
        let origin = ApprovalOrigin::Channel(ChannelKind::Telegram);
        let via = DecisionVia::Channel(ChannelKind::Telegram);
        assert_eq!(serde_json::to_string(&origin).unwrap(), "\"telegram\"");
        assert_eq!(serde_json::to_string(&via).unwrap(), "\"telegram\"");
        assert_eq!(
            serde_json::from_str::<ApprovalOrigin>("\"telegram\"").unwrap(),
            origin
        );
        assert_eq!(
            serde_json::from_str::<DecisionVia>("\"telegram\"").unwrap(),
            via
        );
        assert_eq!(
            serde_yaml::from_str::<ApprovalOrigin>("telegram").unwrap(),
            origin
        );
        assert_eq!(
            serde_yaml::from_str::<DecisionVia>("telegram").unwrap(),
            via
        );
        assert_eq!(
            serde_yaml::from_str::<ApprovalOrigin>("web").unwrap(),
            ApprovalOrigin::Web
        );
        assert_eq!(
            serde_yaml::from_str::<ApprovalOrigin>("background").unwrap(),
            ApprovalOrigin::Background
        );
        assert_eq!(
            serde_yaml::from_str::<DecisionVia>("webui").unwrap(),
            DecisionVia::Webui
        );
        assert_eq!(
            serde_yaml::from_str::<ApprovalOrigin>("matrix").unwrap(),
            ApprovalOrigin::Channel(ChannelKind::Matrix)
        );
        assert_eq!(
            serde_yaml::from_str::<DecisionVia>("matrix").unwrap(),
            DecisionVia::Channel(ChannelKind::Matrix)
        );

        let mut proposal = open_request();
        proposal.origin = Some(origin);
        proposal.decided_via = Some(via);
        let note = proposal.to_note();
        assert!(note.contains("origin: telegram"), "{note}");
        assert!(note.contains("decided_via: telegram"), "{note}");
        assert!(!note.contains("channel:"), "{note}");
        let reloaded = Proposal::from_note(&note).unwrap();
        assert_eq!(reloaded.origin, Some(origin));
        assert_eq!(reloaded.decided_via, Some(via));
    }

    #[test]
    fn cards_follow_the_session_and_background_lands_on_the_sticky_chat() {
        let mut web = open_request();
        web.session_id = Some("web".into());
        web.origin = Some(ApprovalOrigin::Web);
        assert!(card_belongs_to_session(&web, "web", Some("tg")));
        assert!(!card_belongs_to_session(&web, "tg", Some("tg")));

        let mut bg = open_request();
        bg.origin = Some(ApprovalOrigin::Background);
        assert!(card_belongs_to_session(&bg, "tg", Some("tg")));
        assert!(!card_belongs_to_session(&bg, "web", Some("tg")));

        let legacy = open_request();
        assert!(card_belongs_to_session(&legacy, "tg", Some("tg")));
        assert!(!card_belongs_to_session(&legacy, "web", Some("tg")));

        let ordinary = Proposal::pending("p", "c", "s", ProposedAction::ToolCalls(vec![]), "r");
        assert!(!card_belongs_to_session(&ordinary, "tg", Some("tg")));
    }

    #[test]
    fn recorded_action_names_every_stored_state() {
        let now = Utc::now();
        let open = open_request();
        assert_eq!(recorded_action(&open, now), "pending");

        let mut later = open_request();
        later.expires = Some(now + chrono::Duration::seconds(60));
        assert_eq!(recorded_action(&later, now), "pending");

        let mut elapsed = open_request();
        elapsed.expires = Some(now - chrono::Duration::seconds(1));
        assert_eq!(recorded_action(&elapsed, now), "expired");

        let mut expired = open_request();
        expired.status = ProposalStatus::Expired;
        assert_eq!(recorded_action(&expired, now), "expired");

        let mut denied = open_request();
        denied.status = ProposalStatus::Rejected;
        assert_eq!(recorded_action(&denied, now), "deny");

        for status in [ProposalStatus::Approved, ProposalStatus::Done] {
            let mut once = open_request();
            once.status = status;
            once.approved_scope = Some(GrantScope::Once);
            assert_eq!(recorded_action(&once, now), "once");

            let mut session = open_request();
            session.status = status;
            session.approved_scope = Some(GrantScope::Session);
            assert_eq!(recorded_action(&session, now), "session");

            let mut everywhere = open_request();
            everywhere.status = status;
            everywhere.approved_scope = Some(GrantScope::Everywhere);
            assert_eq!(recorded_action(&everywhere, now), "everywhere");

            let mut bare = open_request();
            bare.status = status;
            bare.approved_scope = None;
            assert_eq!(recorded_action(&bare, now), "approved");
        }
    }
}
