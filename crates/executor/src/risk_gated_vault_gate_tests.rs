//! Vault-write gate regressions split from `risk_gated_tests.rs`.
//!
//! These pin the live bug: a session without `Write(Vault("conformance"))` called
//! `turbovault:edit_note` and the gate must raise a permission request.

use super::*;
use liberado_test_support::InvocationRecordingRuntime;

/// Live TurboVault tool surface (2026-10-11): every tool classified via the
/// build-time bundled defaults (see `liberado_common::default_bundled_classification("turbovault")`).
/// The fixture here mirrors what `liberado_config::catalog_from_config` produces for the
/// live node when the operator's `topology.toml` carries the legacy 8-tool `write_tools` and
/// nothing else: the build-time defaults fill the gap so `read_note`/`vector_search` are
/// reads, `edit_note`/`tasks_update`/`manage_tags` are writes, `move_file`/`generate_index`/...
/// carry multi-arg path resolution, `vector_reindex` is vault-wide, `set_active_vault` is
/// admin. See `livetv_with_bundled_defaults_returns_a_fully_classified_descriptor` in
/// `liberado-common`'s catalog tests for the merge that produces this shape.
#[allow(clippy::vec_init_then_push)]
fn livetv_descriptor() -> McpDescriptor {
    // Build by starting from a minimal `McpDescriptor` and applying the bundled-default
    // merge — that's what `liberado_config::catalog_from_config` does in production for
    // any MCP named `turbovault` (or whatever else lands in `BUNDLED_VAULT_MCP_NAMES`).
    // The merge is what makes the live node's old-shape `topology.toml` work without a
    // config change. See `liberado_common::default_bundled_classification("turbovault")`
    // for the build-time source of truth. The merge test lives in `catalog_tests`.
    let minimal = McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: Vec::new(),
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    };
    liberado_common::apply_bundled_classifications(minimal)
}
fn gate_with_grant(
    dir: &std::path::Path,
    inner: Arc<InvocationRecordingRuntime>,
    zones: &[&str],
) -> RiskGatedToolRuntime {
    let mcp = "turbovault".to_string();
    let mut caps = vec![Capability::ExecuteMcp(mcp.clone())];
    for z in zones {
        caps.push(Capability::Write(Zone::vault(*z)));
    }
    let classes = zones
        .iter()
        .map(|z| (z.to_string(), WriteClass::AgentWritable))
        .collect();
    RiskGatedToolRuntime::new(
        inner,
        CapabilitySet::from_iter(caps),
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        classes,
        dir.to_path_buf(),
        "test regression".into(),
        "test-f1-regression".into(),
        ProposalSigner::random(),
        "default",
    )
}

// ── The live bug pinned at the type level ───────────────────────────────

/// `01M3ZB89WK54HGNG6ATJ9FPJMX` from 2026-10-11, exactly: the session had no
/// `Write(Vault("conformance"))` in its grant and called `edit_note` on
/// `conformance/approval-test.md`. The old code returned `NotAWrite` and ran the
/// call. The fixed code returns `Undeterminable` (since `edit_note` is now declared in
/// `write_tools`, the resolution actually proceeds), and the capability gate refuses
/// the call because the grant does not include `Write(Vault("conformance"))`.
#[tokio::test]
async fn edit_note_without_a_zone_grant_is_now_a_permission_request() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:edit_note", "turbovault:read_note"],
        Ok("wrote".into()),
    ));
    // ExecuteMcp only - exactly the Personal Task Manager profile from the live trace.
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        vec![("conformance".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "edit conformance note".into(),
        "test-edit-note-live-bug".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_approval(crate::ApprovalStamp {
        session_id: "01M3ZB89WK54HGNG6ATJ9FPJMX".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    });

    let call = ToolInvocation::new(
        "live-2026-10-11",
        "turbovault:edit_note",
        serde_json::json!({"path": "conformance/approval-test.md"}),
    );
    let msg = rt
        .invoke(&call)
        .await
        .expect("a chat surface raises a permission request, not an error");
    assert!(msg.contains("PERMISSION REQUESTED"), "{msg}");
    assert!(
        msg.contains("conformance"),
        "the request must name the zone the agent can't write: {msg}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "the inner tool must NEVER have run - this is the live-bug assertion: {:?}",
        inner.invoked.lock().unwrap()
    );

    // Proposal file written, with the requested grant.
    let note = std::fs::read_dir(dir.path().join("proposals"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap();
    let proposal =
        liberado_common::Proposal::from_note(&std::fs::read_to_string(note.path()).unwrap())
            .unwrap();
    assert_eq!(
        proposal.session_id.as_deref(),
        Some("01M3ZB89WK54HGNG6ATJ9FPJMX")
    );
}

/// The same call with the `Write(Vault("conformance"))` grant in place - the positive
/// control. Without it, every assertion above would still pass if the gate simply
/// refused *everything* (which would be a catastrophic bug that looks exactly like
/// perfect security).
#[tokio::test]
async fn edit_note_with_a_zone_grant_runs() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:edit_note"],
        Ok("edited".into()),
    ));
    let rt = gate_with_grant(dir.path(), inner.clone(), &["conformance"]);
    let call = ToolInvocation::new(
        "c-positive",
        "turbovault:edit_note",
        serde_json::json!({"path": "conformance/approval-test.md"}),
    );
    let result = rt.invoke(&call).await;
    let result_str: &str = match &result {
        Ok(value) => value.as_str(),
        Err(e) => panic!("granted zone must run, got error: {e}"),
    };
    assert_eq!(result_str, "edited");
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}

// ── Multi-path writers (brief gap A) ────────────────────────────────────────────
//
// Pre-fix: tools with non-`path` args (move_note(from, to), create_from_template(file_path),
// generate_index(directory), append_log_entry(directory), visualize(output)) returned
// `Undeterminable("missing 'path'")` and were always refused. With `write_path_args` set,
// every declared arg resolves to a zone and the runtime requires Write for **each**.

/// Multi-path tool with both zones in the grant — runs.
#[tokio::test]
async fn move_file_with_both_zones_in_grant_runs() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:move_file"],
        Ok("moved".into()),
    ));
    let rt = gate_with_grant(dir.path(), inner.clone(), &["Tasks", "sandbox"]);
    let call = ToolInvocation::new(
        "c-both",
        "turbovault:move_file",
        serde_json::json!({"from": "Tasks/Main.md", "to": "sandbox/Main.md"}),
    );
    let result = rt.invoke(&call).await;
    let result_str: &str = match &result {
        Ok(value) => value.as_str(),
        Err(e) => panic!("both zones granted must run, got error: {e}"),
    };
    assert_eq!(result_str, "moved");
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}

/// Multi-path tool with source in grant but destination NOT — the runtime raises a permission
/// request naming the missing destination zone. The source-side zone gate passes, the
/// destination-side check fails closed.
#[tokio::test]
async fn move_file_with_destination_out_of_grant_requests_the_destination_zone() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:move_file"],
        Ok("moved".into()),
    ));
    let rt = gate_with_grant(dir.path(), inner.clone(), &["Tasks"]).with_approval(
        crate::ApprovalStamp {
            session_id: "sess".into(),
            origin: liberado_common::ApprovalOrigin::Web,
        },
    ); // no sandbox
    let call = ToolInvocation::new(
        "c-mixed",
        "turbovault:move_file",
        serde_json::json!({"from": "Tasks/Main.md", "to": "sandbox/Main.md"}),
    );
    let result = rt.invoke(&call).await.expect("a chat surface must ask");
    assert!(
        result.contains("PERMISSION REQUESTED"),
        "missing destination must card: {result}"
    );
    assert!(
        result.contains("sandbox"),
        "the request must name the missing destination: {result}"
    );
    assert!(
        !result.contains("Tasks"),
        "the source zone IS in the grant and must not be listed as missing: {result}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "and the tool must not have run"
    );
}

/// Multi-path tool with source out of grant, destination in — also requests the source.
/// (Symmetric to the above; pins that the source-side check still works for the
/// destination-known case.)
#[tokio::test]
async fn move_file_with_source_out_of_grant_requests_the_source_zone() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:move_file"],
        Ok("moved".into()),
    ));
    let rt = gate_with_grant(dir.path(), inner.clone(), &["sandbox"]).with_approval(
        crate::ApprovalStamp {
            session_id: "sess".into(),
            origin: liberado_common::ApprovalOrigin::Web,
        },
    ); // no Tasks
    let call = ToolInvocation::new(
        "c-src-out",
        "turbovault:move_file",
        serde_json::json!({"from": "Tasks/Main.md", "to": "sandbox/Main.md"}),
    );
    let result = rt.invoke(&call).await.expect("a chat surface must ask");
    assert!(
        result.contains("PERMISSION REQUESTED"),
        "missing source must card: {result}"
    );
    assert!(
        result.contains("Tasks"),
        "must name the missing source zone: {result}"
    );
}

/// Multi-path tool with both zones out of grant — both must be listed in the message.
#[tokio::test]
async fn move_file_with_both_zones_out_of_grant_lists_both_missing_zones() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:move_file"],
        Ok("moved".into()),
    ));
    let rt = gate_with_grant(dir.path(), inner.clone(), &[]).with_approval(crate::ApprovalStamp {
        session_id: "sess".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    }); // no zones at all
    let call = ToolInvocation::new(
        "c-no-zones",
        "turbovault:move_file",
        serde_json::json!({"from": "Tasks/Main.md", "to": "sandbox/Main.md"}),
    );
    let result = rt.invoke(&call).await.expect("a chat surface must ask");
    assert!(result.contains("PERMISSION REQUESTED"));
    assert!(
        result.contains("Tasks") && result.contains("sandbox"),
        "must name every missing zone: {result}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "and the tool must not have run"
    );
}

/// Multi-path tool without `write_path_args` declared (operator forgot) — falls back to
/// `zone_from_arg` (path). The runtime uses just the source arg and resolves only one
/// zone, even when other path args are present. This documents the fallback so future
/// operators know what happens.
#[tokio::test]
async fn move_file_without_write_path_args_uses_only_the_first_arg() {
    // Set up a descriptor where move_file is in write_tools and zone_from_arg is "from", but
    // no entry in write_path_args. The runtime uses "from" only — destination is invisible.
    let descriptor = McpDescriptor {
        name: "turbovault".into(),
        description: "fallback test".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("from".into()),
        write_tools: vec!["move_file".into()],
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        admin_tools: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(), // move_file NOT declared
    };
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:move_file"],
        Ok("moved".into()),
    ));
    // Only Tasks granted — with the fallback the destination is silent, so the call
    // runs without ever consulting the sandbox grant. (We pin the behaviour, even though
    // it's undesirable — operators should ALWAYS declare write_path_args.)
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        CapabilitySet::from_iter([
            Capability::ExecuteMcp("turbovault".into()),
            Capability::Write(Zone::vault("Tasks")),
        ]),
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![descriptor],
        vec![
            ("Tasks".to_string(), WriteClass::AgentWritable),
            ("sandbox".to_string(), WriteClass::AgentWritable),
        ],
        dir.path().to_path_buf(),
        "move file".into(),
        "test-fallback".into(),
        ProposalSigner::random(),
        "default",
    );
    let call = ToolInvocation::new(
        "c-fallback",
        "turbovault:move_file",
        serde_json::json!({"from": "Tasks/Main.md", "to": "sandbox/Main.md"}),
    );
    let result = rt
        .invoke(&call)
        .await
        .expect("with fallback, the call runs");
    assert_eq!(result, "moved", "with fallback the call runs");
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}

/// Reading tools remain unaffected: an unzoned read on a path-addressed MCP must
/// never ask. If we accidentally promote a read to a write we triple-tap the human
/// for every `read_note`.
#[tokio::test]
async fn read_tools_are_unaffected_by_the_fix() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &[
            "turbovault:read_note",
            "turbovault:search",
            "turbovault:vector_search",
        ],
        Ok("read".into()),
    ));
    let rt = gate_with_grant(dir.path(), inner.clone(), &["Tasks"]);
    for tool in [
        "turbovault:read_note",
        "turbovault:search",
        // vector_search is a read (it queries the vector index; only vector_reindex
        // rebuilds state). With vector_search in the now-correct `read_tools`, a Read-only
        // session can use it without a permission card.
        "turbovault:vector_search",
    ] {
        let call = ToolInvocation::new("c1", tool, serde_json::json!({"path": "Tasks/Main.md"}));
        let result = rt.invoke(&call).await.expect("a read must not ask");
        assert_eq!(&result, "read", "tool={tool}");
    }
    assert_eq!(inner.invoked.lock().unwrap().len(), 3);
}

/// `vector_search` is a read; previously it was classed as `vault_wide_writes` and a
/// Read-only session (no Write(*)) got refused on a vector search. After reclassifying
/// to `read_tools`, a Read-only session can use vector_search without a card.
#[tokio::test]
async fn vector_search_runs_for_a_read_only_session() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:vector_search"],
        Ok("vector-read".into()),
    ));
    // Read-only: ExecuteMcp + Read, no Write at all.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Read(Zone::vault("Tasks")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        vec![("Tasks".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "search the vector index".into(),
        "test-vector-search-read".into(),
        ProposalSigner::random(),
        "default",
    );
    let call = ToolInvocation::new(
        "c-vec",
        "turbovault:vector_search",
        serde_json::json!({"query": "anything"}),
    );
    let result = rt.invoke(&call).await;
    assert_eq!(result.as_ref().map(String::as_str), Ok("vector-read"));
}

/// `manage_tags` and `tasks_update` / `tasks_complete` / `tasks_delete` were missing from
/// the live `write_tools` list (the old list had `update_task` / `delete_task`, which were
/// the previous name and no longer exist). They are now declared writes; each must raise
/// a permission request when the zone isn't in the grant.
#[tokio::test]
async fn the_tools_missing_from_the_old_write_tools_list_are_now_gated() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &[
            "turbovault:manage_tags",
            "turbovault:tasks_update",
            "turbovault:tasks_complete",
            "turbovault:tasks_delete",
        ],
        Ok("done".into()),
    ));
    // No zone grant.
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        vec![("conformance".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "test regression".into(),
        "test-missing-from-old-list".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_approval(crate::ApprovalStamp {
        session_id: "sess".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    });
    for (tool, path) in [
        ("turbovault:manage_tags", "conformance/tags.md"),
        ("turbovault:tasks_update", "conformance/tasks.md"),
        ("turbovault:tasks_complete", "conformance/tasks.md"),
        ("turbovault:tasks_delete", "conformance/tasks.md"),
    ] {
        let call = ToolInvocation::new("c", tool, serde_json::json!({"path": path}));
        let msg = rt
            .invoke(&call)
            .await
            .expect("a write without a grant must ask");
        assert!(
            msg.contains("PERMISSION REQUESTED"),
            "{tool} must now raise a permission request: {msg}"
        );
    }
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "none of the gated tools must have actually run"
    );
}

/// Multi-zone source-and-destination: `move_file(from, to)` resolves two zones, both must
/// be in the grant or the call must raise a permission request that names the missing
/// zones. The old code treated `move_note` / `move_file` as `Undeterminable` (because the
/// arg name was `path` and they don't take `path`) and *always* refused - a hard-coded
/// dead end. The new code resolves both `from` and `to` into zones, checks each against
/// the grant, and refuses if either is missing.
///
/// For this test we use a slim descriptor that explicitly names the multi-path arg so the
/// resolution can find both zones. The full product wiring depends on a runtime schema
/// decision (which args carry paths) that lives in the deployed topology - the test
/// pins the *runtime* behavior, not the deployed topology, which is loaded later by the
/// config layer.
/// Vault-wide mutators (`begin_fanout`, `commit_fanout`, `abandon_fanout`,
/// `set_active_vault`, `vector_reindex`) require the grant to hold at least one
/// `Write(Zone)` capability. A read-only session (the `research` profile) must not be
/// able to start a fanout.
#[tokio::test]
async fn a_vault_wide_write_refuses_a_session_with_no_zone_write_authority() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:begin_fanout"],
        Ok("started".into()),
    ));
    // ExecuteMcp + a Read, but no Write - the read-only session shape.
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Read(Zone::vault("Tasks")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        Vec::new(),
        dir.path().to_path_buf(),
        "start a fanout".into(),
        "test-vault-wide-refuse".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_approval(crate::ApprovalStamp {
        session_id: "sess".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    });
    let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
    let msg = rt
        .invoke(&call)
        .await
        .expect("a chat surface must ask, not error");
    assert!(
        msg.contains("PERMISSION REQUESTED"),
        "the vault-wide gate must raise a permission request: {msg}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "and the tool must not have run"
    );
}

/// Vault-wide mutators with at least ONE zone write authority in the grant - a
/// session that's not strictly read-only - may execute, since the guard asks "this
/// session is not strictly read-only" and the runtime capability check on the actual
/// `Write(Tasks)` alone is **not** enough for a vault-wide mutator like `begin_fanout` —
/// the new strict vault-wide gate requires the explicit `Write(Vault("vault-wide-any"))`
/// marker, the same capability the four-button permission card applies. The original
/// `holds_any_zone_write` semantics was too weak (`Write(Tasks)` was waved through a
/// fanout, which switches the active vault for every concurrent session).
#[tokio::test]
async fn a_vault_wide_write_only_zone_grant_raises_a_permission_request() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:begin_fanout"],
        Ok("started".into()),
    ));
    let mcp = "turbovault".to_string();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp(mcp.clone()),
        Capability::Write(Zone::vault("Tasks")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        vec![("Tasks".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "start a fanout".into(),
        "test-vault-wide-only-zone".into(),
        ProposalSigner::random(),
        "default",
    )
    .with_approval(crate::ApprovalStamp {
        session_id: "sess".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    });
    let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
    let result = rt.invoke(&call).await.expect("a chat surface must ask");
    assert!(
        result.contains("PERMISSION REQUESTED"),
        "Write(Tasks) alone must NOT authorise a fanout: {result}"
    );
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "and the tool must not have run"
    );
}

/// The **explicit** vault-wide marker grants the operation. Same shape as the marker the
/// four-button permission request writes — the chat approval applies via Session/Everywhere
/// and is observable here as a directly-held capability on the gate's capability set.
#[tokio::test]
async fn a_vault_wide_write_with_the_marker_grant_runs() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:begin_fanout"],
        Ok("started".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault(liberado_common::VAULT_WIDE_ZONE_MARKER)),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        Vec::new(),
        dir.path().to_path_buf(),
        "start a fanout".into(),
        "test-vault-wide-marker".into(),
        ProposalSigner::random(),
        "default",
    );
    let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
    let result = rt.invoke(&call).await;
    let result_str: &str = match &result {
        Ok(value) => value.as_str(),
        Err(e) => panic!("the explicit marker grant authorises the fanout, got: {e}"),
    };
    assert_eq!(result_str, "started");
    assert_eq!(inner.invoked.lock().unwrap().len(), 1);
}

/// The staleness hole pinned: the deployed topology.toml named `update_task` and
/// `delete_task` (the OLD turbovault API), and the new task tools (`tasks_update`,
/// `tasks_delete`) silently sailed through every gate as NotAWrite. After the fix,
/// undeclared tools are refused with a remediation hint rather than waved through.
#[tokio::test]
async fn an_undeclared_tool_on_a_path_addressed_mcp_is_refused_with_a_hint() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:brand_new_tool_added_in_a_future_release"],
        Ok("ran".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("Tasks")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        vec![("Tasks".to_string(), WriteClass::AgentWritable)],
        dir.path().to_path_buf(),
        "do the new thing".into(),
        "test-undeclared".into(),
        ProposalSigner::random(),
        "default",
    );
    let call = ToolInvocation::new(
        "c1",
        "turbovault:brand_new_tool_added_in_a_future_release",
        serde_json::json!({"path": "Tasks/x.md"}),
    );
    let err = rt
        .invoke(&call)
        .await
        .expect_err("an undeclared tool must be REFUSED, not waved through");
    // The error names both the tool and the three classification lists, so an operator
    // reading it can fix the topology without running the daemon.
    assert!(
        err.contains("brand_new_tool_added_in_a_future_release"),
        "{err}"
    );
    assert!(err.contains("write_tools"), "{err}");
    assert!(err.contains("read_tools"), "{err}");
    assert!(err.contains("vault_wide_writes"), "{err}");
    assert!(
        inner.invoked.lock().unwrap().is_empty(),
        "the inner tool must never have run"
    );
}

/// The Once / Session / Everywhere resolution flows through `set_active_vault` exactly
/// like a per-zone write - same proposal shape, same buttons. A background session
/// (no approval stamp, no notifier) gets the hard refusal with no proposal at all.
#[tokio::test]
async fn a_vault_wide_write_in_a_background_run_is_refused_without_a_proposal() {
    let dir = tempfile::TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:begin_fanout"],
        Ok("started".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Read(Zone::vault("Tasks")),
    ]);
    // No approval stamp, no notifier - a background cron.
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![livetv_descriptor()],
        Vec::new(),
        dir.path().to_path_buf(),
        "start a fanout unattended".into(),
        "test-vault-wide-background".into(),
        ProposalSigner::random(),
        "default",
    );
    let call = ToolInvocation::new("c1", "turbovault:begin_fanout", serde_json::json!({}));
    let err = rt.invoke(&call).await.expect_err(
        "a background run must error, not raise a permission request nobody can answer",
    );
    assert!(err.contains("not authorized"), "{err}");
    assert!(err.contains("Write"), "{err}");
    // And no proposal file is written.
    let prop_dir = dir.path().join("proposals");
    assert!(
        !prop_dir.exists() || std::fs::read_dir(&prop_dir).unwrap().next().is_none(),
        "no permission request must be created in a background run"
    );
}
