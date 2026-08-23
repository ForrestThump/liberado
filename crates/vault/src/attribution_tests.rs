//! Split from `attribution.rs` for module-health boundaries.

use crate::{Attribution, Vault, WriteProvenance};
use tempfile::TempDir;

async fn temp_vault() -> (Vault, TempDir) {
    let dir = TempDir::new().unwrap();
    let vault = Vault::open("test", dir.path()).await.unwrap();
    (vault, dir)
}

#[tokio::test]
async fn agent_write_is_attributed_and_suppressed() {
    let (vault, _dir) = temp_vault().await;
    let prov = WriteProvenance::agent("daily-review-agent", "review-1").with_zone("reviews");

    vault
        .write("reviews/2026-06-21.md", "# Review\nbody", None, &prov)
        .await
        .unwrap();

    match vault.attribute("reviews/2026-06-21.md").await.unwrap() {
        Attribution::Agent(p) => assert_eq!(p.correlation_id.as_deref(), Some("review-1")),
        other => panic!("expected Agent attribution, got {other:?}"),
    }
    assert!(!vault.should_react("reviews/2026-06-21.md").await.unwrap());
}

#[tokio::test]
async fn human_edit_after_agent_write_is_reacted_to() {
    let (vault, dir) = temp_vault().await;
    let prov = WriteProvenance::agent("tasks-mcp", "task-1");

    vault
        .write("tasks/today.md", "- [ ] original", None, &prov)
        .await
        .unwrap();
    // Agent write is suppressed...
    assert!(!vault.should_react("tasks/today.md").await.unwrap());

    // ...then a human edits the note directly in Obsidian (a raw file write, not through the
    // adapter, so no audit entry with a matching after_hash exists).
    std::fs::write(dir.path().join("tasks/today.md"), "- [x] done by hand").unwrap();

    assert_eq!(
        vault.attribute("tasks/today.md").await.unwrap(),
        Attribution::External
    );
    assert!(vault.should_react("tasks/today.md").await.unwrap());
}

#[tokio::test]
async fn latest_matching_write_wins() {
    let (vault, _dir) = temp_vault().await;

    let h1 = Vault::content_hash("v1");
    vault
        .write("notes/n.md", "v1", None, &WriteProvenance::agent("a", "c1"))
        .await
        .unwrap();
    // Second agent overwrites; current content is now "v2", attributed to the second write.
    vault
        .write(
            "notes/n.md",
            "v2",
            Some(&h1),
            &WriteProvenance::agent("b", "c2"),
        )
        .await
        .unwrap();

    match vault.attribute("notes/n.md").await.unwrap() {
        Attribution::Agent(p) => {
            assert_eq!(p.source, "b");
            assert_eq!(p.correlation_id.as_deref(), Some("c2"));
        }
        other => panic!("expected Agent(b), got {other:?}"),
    }
}

#[tokio::test]
async fn external_recreation_of_a_moved_source_is_not_falsely_suppressed() {
    // Regression: a Move records `after_hash` under its *source* path but for the
    // *destination's* content. If attribution matched the source path, a later human
    // recreation of that source with the moved content would be wrongly suppressed.
    let (vault, dir) = temp_vault().await;

    // A human-authored note (no audit entry of its own)...
    std::fs::write(dir.path().join("orig.md"), "shared content").unwrap();
    // ...is filed away by an agent (the only audit entry mentioning `orig.md` is this move).
    vault
        .move_note(
            "orig.md",
            "filed.md",
            None,
            &WriteProvenance::agent("organizer", "mv-1"),
        )
        .await
        .unwrap();

    // The human recreates the original path with that same content.
    std::fs::write(dir.path().join("orig.md"), "shared content").unwrap();

    // It must be treated as an external edit (react), not the agent's move.
    assert_eq!(
        vault.attribute("orig.md").await.unwrap(),
        Attribution::External
    );
    // ...while the destination is still correctly attributed to the agent.
    assert!(matches!(
        vault.attribute("filed.md").await.unwrap(),
        Attribution::Agent(_)
    ));
}

#[tokio::test]
async fn human_sourced_write_is_not_suppressed() {
    let (vault, _dir) = temp_vault().await;
    // Even a write that went through the audit log but is tagged source=human must be reacted
    // to — attribution suppresses only *our agents'* writes.
    let human = WriteProvenance {
        source: "human".into(),
        correlation_id: None,
        zone: None,
        note: None,
    };
    vault
        .write("journal/today.md", "dear diary", None, &human)
        .await
        .unwrap();

    assert_eq!(
        vault.attribute("journal/today.md").await.unwrap(),
        Attribution::External
    );
}

/// Reproduce the shape an MCP tool write leaves behind: the bytes are on disk and the audit log
/// has an entry whose `after_hash` matches them, but `metadata` is `{}` because the tool layer
/// dropped our `_meta`. This is what `turbovault:write_note` actually produced for the morning
/// brief on 2026-07-27 (verified against the live audit log).
async fn write_like_an_mcp_tool(vault: &Vault, dir: &TempDir, rel: &str, content: &str) {
    let path = dir.path().join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();

    let hash = Vault::content_hash(content);
    let entry = turbovault_audit::AuditEntry::new(turbovault_audit::OperationType::Create, rel)
        .with_after(hash.clone(), hash);
    assert_eq!(
        entry.metadata,
        serde_json::json!({}),
        "the bug depends on this entry carrying no provenance"
    );
    vault.audit().record(&entry).await.unwrap();
}

/// The regression: a delivered report is suppressed even though the audit entry lost its
/// provenance, because the note's own front matter still names the agent that wrote it.
#[tokio::test]
async fn mcp_tool_write_is_attributed_from_front_matter() {
    let (vault, dir) = temp_vault().await;
    let note = "---\nliberado_source: liberado-executor\n\
                    liberado_correlation: sub:6dba0afc21695ad7\n\
                    generated: 2026-07-27T11:55:38Z\n---\n\n# Morning Brief\n\nbody\n";

    write_like_an_mcp_tool(&vault, &dir, "wakeups/2026-07-27-morning-brief.md", note).await;

    match vault
        .attribute("wakeups/2026-07-27-morning-brief.md")
        .await
        .unwrap()
    {
        Attribution::Agent(p) => {
            assert_eq!(p.source, "liberado-executor");
            assert_eq!(p.correlation_id.as_deref(), Some("sub:6dba0afc21695ad7"));
        }
        other => panic!("expected the delivered report to be suppressed, got {other:?}"),
    }
    assert!(
        !vault
            .should_react("wakeups/2026-07-27-morning-brief.md")
            .await
            .unwrap()
    );
}

/// The guarantee the fallback must not break. A human edits a note that still carries agent
/// front matter; their bytes match no audit entry, so the stale front matter is never read and
/// the edit is still reacted to. This is the one mistake loop-breaking may never make.
#[tokio::test]
async fn human_edit_of_a_note_with_agent_front_matter_still_reacts() {
    let (vault, dir) = temp_vault().await;
    let note = "---\nliberado_source: liberado-executor\nliberado_correlation: c1\n---\n\nbody\n";

    write_like_an_mcp_tool(&vault, &dir, "wakeups/brief.md", note).await;
    assert!(!vault.should_react("wakeups/brief.md").await.unwrap());

    // The human appends a line in Obsidian. Front matter still says liberado-executor.
    std::fs::write(
        dir.path().join("wakeups/brief.md"),
        format!("{note}\nmy own note\n"),
    )
    .unwrap();

    assert_eq!(
        vault.attribute("wakeups/brief.md").await.unwrap(),
        Attribution::External,
        "a human edit must be reacted to even when stale agent front matter remains"
    );
}

/// Front matter must not override an audit entry that explicitly names a human.
#[tokio::test]
async fn audit_human_provenance_beats_agent_front_matter() {
    let (vault, _dir) = temp_vault().await;
    let note = "---\nliberado_source: liberado-executor\n---\n\napproved by hand\n";

    vault
        .write("proposals/p.md", note, None, &WriteProvenance::human())
        .await
        .unwrap();

    assert_eq!(
        vault.attribute("proposals/p.md").await.unwrap(),
        Attribution::External
    );
}

/// A note whose front matter names no liberado source is still external, even via the MCP path.
#[tokio::test]
async fn mcp_tool_write_without_liberado_front_matter_is_external() {
    let (vault, dir) = temp_vault().await;

    write_like_an_mcp_tool(&vault, &dir, "notes/n.md", "---\ntags: [x]\n---\n\nbody\n").await;
    assert_eq!(
        vault.attribute("notes/n.md").await.unwrap(),
        Attribution::External
    );

    // ...and so is a note with no front matter at all.
    write_like_an_mcp_tool(&vault, &dir, "notes/plain.md", "just text\n").await;
    assert_eq!(
        vault.attribute("notes/plain.md").await.unwrap(),
        Attribution::External
    );
}

/// Human-sourced front matter must not be attributed as Agent via the MCP fallback path.
/// The `!prov.is_human()` guard on the front matter branch must reject human-sourced entries,
/// not just the audit entry branch. This is the regression test for the mutation that replaces
/// the guard with `true`.
#[tokio::test]
async fn mcp_tool_write_with_human_front_matter_is_external_not_agent() {
    let (vault, dir) = temp_vault().await;
    let note = "---\nliberado_source: human\n---\n\napproved by hand\n";

    write_like_an_mcp_tool(&vault, &dir, "proposals/approved.md", note).await;

    // The front matter says `human`, so even though the audit entry has no metadata
    // (MCP path), the `!prov.is_human()` guard must reject it and fall through to External.
    assert_eq!(
        vault.attribute("proposals/approved.md").await.unwrap(),
        Attribution::External,
        "human-sourced front matter via MCP path must be External, not Agent"
    );
}

#[test]
fn front_matter_parsing_edge_cases() {
    // Unterminated block: refuse to mine the body for provenance.
    assert!(
        super::frontmatter_provenance("---\nliberado_source: liberado-executor\n\nbody\n")
            .is_none()
    );
    // Must open on the first line.
    assert!(
        super::frontmatter_provenance("\n---\nliberado_source: liberado-executor\n---\n").is_none()
    );
    // Quotes are stripped; a missing correlation id is allowed (source is what decides).
    let p = super::frontmatter_provenance("---\nliberado_source: \"agent-x\"\n---\n").unwrap();
    assert_eq!(p.source, "agent-x");
    assert_eq!(p.correlation_id, None);
    // An empty source is not provenance.
    assert!(super::frontmatter_provenance("---\nliberado_source:\n---\n").is_none());
    // A human-sourced note is parsed, and `is_human` is what suppresses suppression.
    let h = super::frontmatter_provenance("---\nliberado_source: human\n---\n").unwrap();
    assert!(h.is_human());
}

#[tokio::test]
async fn missing_path_is_missing() {
    let (vault, _dir) = temp_vault().await;
    assert_eq!(
        vault.attribute("does/not/exist.md").await.unwrap(),
        Attribution::Missing
    );
}
