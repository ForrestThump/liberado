use chrono::{DateTime, Utc};
use liberado_common::{Capability, Proposal, ProposalStatus, ProposedAction, ToolCall, Zone};

use super::pending_approval_row;

fn at(stamp: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .with_timezone(&Utc)
}

fn permission(id: &str) -> Proposal {
    let mut proposal = Proposal::pending(
        id,
        "corr",
        "liberado-chat",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: "turbovault:edit_note".into(),
            args: serde_json::json!({}),
        }]),
        "needs write",
    )
    .with_requested_grant(Capability::Write(Zone::vault("tasks")));
    proposal.session_id = Some("sess-1".into());
    proposal.created = at("2026-10-01T00:00:00Z");
    proposal
}

#[test]
fn open_permission_row_carries_id_conversation_tool_zone_and_time() {
    let now = at("2026-10-12T00:00:00Z");
    let row = pending_approval_row(&permission("perm-1"), now).unwrap();
    assert_eq!(row.id, "perm-1");
    assert_eq!(row.conversation.as_deref(), Some("sess-1"));
    assert_eq!(row.tool.as_deref(), Some("turbovault:edit_note"));
    assert_eq!(row.zone.as_deref(), Some("tasks"));
    assert!(row.created_at.contains("2026-10-01"));
}

#[test]
fn decided_ordinary_and_expired_notes_are_not_rows() {
    let now = at("2026-10-12T00:00:00Z");
    let mut decided = permission("perm-done");
    decided.status = ProposalStatus::Approved;
    assert!(pending_approval_row(&decided, now).is_none());

    let ordinary = Proposal::pending(
        "prop-1",
        "corr",
        "liberado-chat",
        ProposedAction::External {
            description: "send".into(),
        },
        "ordinary",
    );
    assert!(pending_approval_row(&ordinary, now).is_none());

    let mut expired = permission("perm-late");
    expired.expires = Some(at("2026-10-02T00:00:00Z"));
    assert!(pending_approval_row(&expired, now).is_none());
}
