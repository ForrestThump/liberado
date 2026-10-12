use chrono::{Duration, Utc};
use liberado_common::{
    Capability, Proposal, ProposalStatus, ProposedAction, ToolCall, WriteProvenance, Zone,
};
use liberado_vault::Vault;

use super::super::PermissionResolver;

fn permission(id: &str, tool: &str, zone: &str) -> Proposal {
    Proposal::pending(
        id,
        "corr",
        "liberado-chat",
        ProposedAction::ToolCalls(vec![ToolCall {
            tool: tool.to_string(),
            args: serde_json::json!({}),
        }]),
        "needs write",
    )
    .with_requested_grant(Capability::Write(Zone::vault(zone)))
}

async fn write_note(vault: &Vault, proposal: &Proposal) {
    let path = format!("proposals/{}.md", proposal.id);
    vault
        .write(&path, &proposal.to_note(), None, &WriteProvenance::human())
        .await
        .unwrap();
}

#[tokio::test]
async fn lists_open_permission_requests_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    let mut older = permission("perm-old", "turbovault:edit_note", "tasks");
    older.session_id = Some("sess-a".into());
    older.created -= Duration::seconds(90);
    let newer = permission("perm-new", "turbovault:move_note", "work");
    write_note(&vault, &older).await;
    write_note(&vault, &newer).await;

    let mut decided = permission("perm-done", "turbovault:edit_note", "tasks");
    decided.status = ProposalStatus::Approved;
    write_note(&vault, &decided).await;

    let ordinary = Proposal::pending(
        "prop-plain",
        "corr",
        "liberado-chat",
        ProposedAction::External {
            description: "send".into(),
        },
        "ordinary",
    );
    write_note(&vault, &ordinary).await;

    let mut expired = permission("perm-old-clock", "turbovault:edit_note", "tasks");
    expired.expires = Some(Utc::now() - Duration::seconds(5));
    write_note(&vault, &expired).await;

    let archive = dir.path().join("proposals/archive/approved");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join("perm-archived.md"), older.to_note()).unwrap();
    std::fs::write(dir.path().join("proposals/garbage.md"), "not a note").unwrap();

    let listed = PermissionResolver::new(vault, None)
        .list_pending_permission_requests()
        .await;
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert_eq!(listed[0].id, "perm-old");
    assert_eq!(listed[0].session_id.as_deref(), Some("sess-a"));
    assert_eq!(listed[1].id, "perm-new");
    assert!(listed[1].session_id.is_none());
}

#[tokio::test]
async fn missing_proposals_dir_is_an_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    let listed = PermissionResolver::new(vault, None)
        .list_pending_permission_requests()
        .await;
    assert!(listed.is_empty());
}
