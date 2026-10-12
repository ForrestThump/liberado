//! Live-arrival test for the OLD-shape `topology.toml` that ships on the homelab node.
//!
//! The live node still carries the pre-2026-10-11 turbovault block — `zone_from_arg = "path"`,
//! the legacy 8-tool `write_tools`, no `read_tools` / `vault_wide_writes` / `write_path_args` /
//! `admin_tools`. With the runtime's fail-closed shape (a path-addressed MCP must classify every
//! tool in one of the three lists or it returns `Undeterminable`), that legacy file would refuse
//! every call. The fix in this branch — `apply_bundled_classifications` in `liberado_common` —
//! fills the gaps from build-time defaults (every tool in the live server's `tools/list`).
//! This test loads the exact block and exercises the runtime against the resulting
//! `McpDescriptor`, asserting the live-bug cases from the bug report:
//!
//!   * reads (`read_note` / `search` / `tasks_list` / `vector_search`) DON'T raise a card
//!   * `edit_note` / `tasks_update` / `manage_tags` on an ungranted zone raise the permission
//!     request
//!   * `move_file`'s destination zone is checked (mixed-zone move triggers a card)
//!   * undeclared tools refuse (an `edit_note`-shaped tool the operator hasn't classified
//!     gets a refusal with a hint, not a free pass)
//!   * `set_active_vault` is admin (hard deny, no card)
//!   * `vector_reindex` is vault-wide (needs the marker grant; "any write" is NOT enough)

use super::load_config;
use liberado_common::{
    Capability, CapabilitySet, Consequence, McpDescriptor, VAULT_WIDE_ZONE_MARKER, Zone,
};
use liberado_executor::{ApprovalStamp, RiskGatedToolRuntime, ToolRuntime};
use liberado_provider::ToolInvocation;
use liberado_test_support::InvocationRecordingRuntime;
use std::sync::Arc;
use tempfile::TempDir;

/// The legacy `[[mcps]]` block that the live homelab node has shipped since before
/// 2026-10-11. Taken from `notes/liberado/vault-gate/live-turbovault-block.toml`. Do not
/// update this constant on autopilot — if the live node's `topology.toml` doesn't look like
/// this, the merged fix has not landed. Update this test in the same PR that updates the
/// live config to keep the regression contract anchored.
const LIVE_TURBOVAULT_BLOCK: &str = r#"
[[mcps]]
name = "turbovault"
description = "Obsidian vault ops via TurboVault: notes, search, links, templates, batch edits"
consequence = "reversible"
transport = { kind = "http", url = "http://turbovault:3001" }
zone_from_arg = "path"
write_tools = [
    "write_note",
    "delete_note",
    "move_note",
    "move_file",
    "update_frontmatter",
    "create_from_template",
    "update_task",
    "delete_task",
]
"#;

const POLICY_BLOCK: &str = r#"
[[zones]]
zone = "Tasks"
write_class = "agent_writable"

[[zones]]
zone = "Work"
write_class = "agent_writable"

[[grants]]
component = "personal-task-manager"
capabilities = [
    { ExecuteMcp = "turbovault" },
    { Read = { Vault = "Tasks" } },
]
"#;

/// Build a `RiskGatedToolRuntime` from the exact legacy shape — verifying the runtime can be
/// constructed against the live config's descriptor (the merged defaults fill the gaps that
/// used to leave every turboVault call in `Undeterminable`).
fn load_live() -> (crate::Config, McpDescriptor) {
    let _guard = match crate::survivor_tests::env_lock().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let data = TempDir::new().unwrap();
    let _env = crate::survivor_tests::EnvGuard::set("LIBERADO_DATA_DIR", data.path());

    let config_dir = TempDir::new().unwrap();
    let vault = config_dir.path().join("vault");
    std::fs::create_dir_all(&vault).unwrap();

    let topology = format!("vault_path = \"{}\"\n", vault.display()) + LIVE_TURBOVAULT_BLOCK;
    std::fs::write(config_dir.path().join(super::TOPOLOGY_FILE), topology).unwrap();
    std::fs::write(config_dir.path().join(super::POLICY_FILE), POLICY_BLOCK).unwrap();

    let (config, _) = load_config(Some(config_dir.path())).expect("legacy block must load");
    // `catalog_from_config` is the production path: toml -> McpConfig -> McpDescriptor,
    // with bundled defaults applied so the legacy block still classifies every tool.
    let merged = super::catalog_from_config(&config)
        .into_iter()
        .find(|descriptor| descriptor.name == "turbovault")
        .expect("turbovault must be in the catalog");
    (config, merged)
}

fn live_loaded_descriptor() -> McpDescriptor {
    load_live().1
}

fn runtime_for_grant(
    capabilities: CapabilitySet,
    proposals_dir: &std::path::Path,
    approval: bool,
) -> RiskGatedToolRuntime {
    let merged = live_loaded_descriptor();
    let consequence = vec![(merged.name.clone(), merged.consequence)];
    let classes = vec![
        (
            "Tasks".to_string(),
            liberado_common::WriteClass::AgentWritable,
        ),
        (
            "Work".to_string(),
            liberado_common::WriteClass::AgentWritable,
        ),
        (
            "conformance".to_string(),
            liberado_common::WriteClass::AgentWritable,
        ),
    ];
    let mut rt = RiskGatedToolRuntime::new(
        Arc::new(InvocationRecordingRuntime::new(
            &[
                "turbovault:read_note",
                "turbovault:edit_note",
                "turbovault:vector_search",
                "turbovault:move_file",
                "turbovault:generate_index",
                "turbovault:set_active_vault",
                "turbovault:vector_reindex",
                "turbovault:begin_fanout",
                // The legacy operator-listed writers from the live block:
                "turbovault:write_note",
                "turbovault:delete_note",
                "turbovault:move_note",
                "turbovault:update_frontmatter",
                "turbovault:create_from_template",
                "turbovault:update_task", // stale name — live server has no such tool
                "turbovault:delete_task", // stale name
            ],
            Ok("ok".into()),
        )),
        capabilities,
        consequence,
        vec![merged],
        classes,
        proposals_dir.to_path_buf(),
        "live-tv".into(),
        "live-arrival".into(),
        liberado_common::proposal::ProposalSigner::random(),
        "default",
    );
    if approval {
        rt = rt.with_approval(ApprovalStamp {
            session_id: "live-tv".into(),
            origin: liberado_common::ApprovalOrigin::Web,
        });
    }
    rt
}

/// The legacy operator-listed `write_tools` is silently extended by the bundled defaults —
/// the runtime can resolve *every* call, including the post-fix `edit_note` /
/// `tasks_update` / `manage_tags` set.
#[test]
fn the_live_legacy_block_loads_with_every_tool_classified() {
    let (config, merged) = load_live();
    let raw = config
        .topology
        .mcps
        .iter()
        .find(|mcp| mcp.name == "turbovault")
        .expect("the legacy block parsed");
    // The file on disk still has only the 8-tool write_tools. The catalog merge, not the
    // file, is what classifies `edit_note` / `tasks_update` / `vector_search`.
    assert!(raw.read_tools.is_empty(), "the live file has no read_tools");
    assert_eq!(raw.write_tools.len(), 8);
    assert!(raw.admin_tools.is_empty());

    // Post-merge, every tool the live server exposes is in one of the four lists.
    for tool in [
        "edit_note",
        "tasks_update",
        "manage_tags",
        "tasks_complete",
        "tasks_delete",
        "generate_index",
        "append_log_entry",
        "visualize",
        "read_note",
        "search",
        "tasks_list",
        "vector_search",
        "vector_status",
        "vector_config",
        "begin_fanout",
        "commit_fanout",
        "abandon_fanout",
        "vector_reindex",
        "set_active_vault",
    ] {
        let hits = [
            merged.write_tools.iter().any(|name| name == tool),
            merged.read_tools.iter().any(|name| name == tool),
            merged.vault_wide_writes.iter().any(|name| name == tool),
            merged.admin_tools.iter().any(|name| name == tool),
        ]
        .into_iter()
        .filter(|hit| *hit)
        .count();
        assert_eq!(
            hits, 1,
            "tool '{tool}' must be in exactly one classification list"
        );
    }
    // Stale names from the legacy 8-tool list (update_task / delete_task) are harmless —
    // they're listed in write_tools, but the live server has no such tool today. The runtime
    // doesn't see them called.
    assert!(merged.write_tools.contains(&"update_task".to_string()));
    assert!(merged.write_tools.contains(&"delete_task".to_string()));
}

#[tokio::test]
async fn a_read_does_not_raise_a_permission_card() {
    let proposals = TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &[
            "turbovault:read_note",
            "turbovault:search",
            "turbovault:tasks_list",
            "turbovault:vector_search",
        ],
        Ok("read-ok".into()),
    ));
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![live_loaded_descriptor()],
        vec![
            (
                "Tasks".to_string(),
                liberado_common::WriteClass::AgentWritable,
            ),
            (
                "Work".to_string(),
                liberado_common::WriteClass::AgentWritable,
            ),
        ],
        proposals.path().to_path_buf(),
        "test".into(),
        "test".into(),
        liberado_common::proposal::ProposalSigner::random(),
        "default",
    )
    .with_approval(ApprovalStamp {
        session_id: "live-tv".into(),
        origin: liberado_common::ApprovalOrigin::Web,
    });
    for tool in [
        "turbovault:read_note",
        "turbovault:search",
        "turbovault:tasks_list",
        "turbovault:vector_search",
    ] {
        let call = ToolInvocation::new("c", tool, serde_json::json!({"path": "Tasks/Main.md"}));
        let result = rt
            .invoke(&call)
            .await
            .expect("a read must not raise a card");
        assert_eq!(result, "read-ok", "tool={tool}");
    }
    // No proposal files written.
    let proposals_dir = proposals.path().join("proposals");
    let count = std::fs::read_dir(&proposals_dir)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(count, 0, "no permission cards may be raised for reads");
}

#[tokio::test]
async fn edit_note_on_an_ungranted_zone_raises_a_permission_request() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new(
        "c",
        "turbovault:edit_note",
        serde_json::json!({"path": "conformance/approval-test.md"}),
    );
    let result = rt.invoke(&call).await.expect("a chat surface must ask");
    assert!(result.contains("PERMISSION REQUESTED"), "{result}");
    // The proposal file says `requested_grant = Write(Vault("conformance"))`.
    let entries: Vec<_> = std::fs::read_dir(proposals.path().join("proposals"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 1);
    let bytes = std::fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
    let proposal = liberado_common::Proposal::from_note(&bytes).unwrap();
    assert_eq!(
        proposal.requested_grant,
        Some(Capability::Write(Zone::vault("conformance"))),
        "the proposal asks for the missing zone"
    );
}

#[tokio::test]
async fn tasks_update_raises_a_permission_request_on_the_live_legacy_grant() {
    // The legacy block lists `update_task` (the OLD task tool name), NOT `tasks_update`.
    // The runtime's bundled defaults ADD `tasks_update`; with the merged set, a session
    // that holds only `Read(Tasks)` (the legacy Personal Task Manager profile) still has to
    // ask to call `tasks_update` on a zone it can't authorise.
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Read(Zone::vault("Tasks")),
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new(
        "c",
        "turbovault:tasks_update",
        serde_json::json!({"path": "Tasks/x.md"}),
    );
    let result = rt.invoke(&call).await.expect("must ask");
    assert!(result.contains("PERMISSION REQUESTED"), "{result}");
    assert!(
        result.contains("Tasks"),
        "the request must name Tasks as the missing zone: {result}"
    );
}

#[tokio::test]
async fn manage_tags_on_an_ungranted_zone_raises_a_permission_request() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new(
        "c",
        "turbovault:manage_tags",
        serde_json::json!({"path": "Work/note.md"}),
    );
    let result = rt.invoke(&call).await.expect("must ask");
    assert!(result.contains("PERMISSION REQUESTED"), "{result}");
    assert!(result.contains("Work"), "{result}");
}

/// `move_file` carries TWO path args. The bundled `write_path_args` configures it with
/// `from` + `to`. Both zones must be authorised — moving Tasks → sandbox with only
/// `Write(Tasks)` in the grant still raises a card naming `sandbox`.
#[tokio::test]
async fn move_file_destination_zone_is_checked_at_runtime() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("Tasks")), // only Tasks
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new(
        "c",
        "turbovault:move_file",
        serde_json::json!({"from": "Tasks/Main.md", "to": "sandbox/Main.md"}),
    );
    let result = rt.invoke(&call).await.expect("must ask");
    assert!(
        result.contains("PERMISSION REQUESTED"),
        "mixed-zone move must card: {result}"
    );
    assert!(
        result.contains("sandbox"),
        "the missing destination zone must be named: {result}"
    );
}

/// Undeclared tools refuse (live config has gaps the runtime catches if the operator's config
/// is still mid-update). A tool that the bundled defaults don't cover AND that the operator's
/// config doesn't list is `Undeterminable`.
#[tokio::test]
async fn undeclared_tool_is_refused_with_remediation_hint() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("Tasks")),
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), false); // no approval — background
    let call = ToolInvocation::new(
        "c",
        "turbovault:future_tool_added_in_a_future_turbovault_release",
        serde_json::json!({"path": "Tasks/x.md"}),
    );
    let err = rt.invoke(&call).await.expect_err(
        "an undeclared tool on a path-addressed MCP must be refused, not waved through",
    );
    assert!(err.contains("not authorized"), "{err}");
    assert!(err.contains("future_tool_added"), "{err}");
    assert!(
        err.contains("write_tools") || err.contains("read_tools"),
        "the refusal must name the three classification lists: {err}"
    );
}

/// `set_active_vault` is **admin** — not a vault-wide write. The runtime hard-denies the
/// call regardless of grants; no permission card surfaces (an admin operation's "approval"
/// is the operator, not the chat).
#[tokio::test]
async fn set_active_vault_is_admin_and_hard_denied() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("Tasks")),
        Capability::Write(Zone::vault(VAULT_WIDE_ZONE_MARKER)), // even the marker
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new("c", "turbovault:set_active_vault", serde_json::json!({}));
    let err = rt
        .invoke(&call)
        .await
        .expect_err("set_active_vault is admin; the runtime hard-denies — no permission card");
    assert!(err.contains("not authorized"), "{err}");
    assert!(
        err.contains("admin-only") || err.contains("admin tool"),
        "refusal must explain why admin tools can't be invoked from a chat: {err}"
    );
    // No permission proposal is written — there's no card surface to write to.
    let proposals_dir = proposals.path().join("proposals");
    let count = std::fs::read_dir(&proposals_dir)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(
        count, 0,
        "admin tools must NOT write a permission proposal — that's the whole point"
    );
}

/// Vector reads (`vector_search` / `vector_status` / `vector_config`) are reads, not
/// vault-wide writes. The legacy live file had them in `vault_wide_writes`; the fix moved
/// them to `read_tools`. A Read-only session can now query the vector index without a card.
#[tokio::test]
async fn vector_reads_run_for_a_read_only_session() {
    let proposals = TempDir::new().unwrap();
    let inner = Arc::new(InvocationRecordingRuntime::new(
        &["turbovault:vector_search"],
        Ok("vec-ok".into()),
    ));
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Read(Zone::vault("Tasks")),
    ]);
    let rt = RiskGatedToolRuntime::new(
        inner.clone(),
        caps,
        vec![("turbovault".into(), Consequence::Reversible)],
        vec![live_loaded_descriptor()],
        vec![(
            "Tasks".to_string(),
            liberado_common::WriteClass::AgentWritable,
        )],
        proposals.path().to_path_buf(),
        "test".into(),
        "test".into(),
        liberado_common::proposal::ProposalSigner::random(),
        "default",
    );
    let call = ToolInvocation::new(
        "c",
        "turbovault:vector_search",
        serde_json::json!({"query": "anything"}),
    );
    let result = rt
        .invoke(&call)
        .await
        .expect("vector_search must run for a Read-only session");
    assert_eq!(result, "vec-ok");
}

/// Vault-wide writes (`vector_reindex`, fanouts) require the **explicit** marker grant —
/// not any `Write(zone)`. A session with only `Write(Tasks)` cannot rebuild the index.
/// (Replacement for the bug-class "any zone write authorises vault-wide".)
#[tokio::test]
async fn vector_reindex_with_only_write_tasks_is_refused() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("Tasks")), // no marker
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new("c", "turbovault:vector_reindex", serde_json::json!({}));
    let result = rt.invoke(&call).await.expect("a chat surface must ask");
    assert!(
        result.contains("PERMISSION REQUESTED"),
        "Tasks-only grant must NOT authorise a vault-wide rebuild: {result}"
    );
    assert!(
        result.contains(VAULT_WIDE_ZONE_MARKER),
        "the request must name the marker: {result}"
    );
}

#[tokio::test]
async fn vector_reindex_with_the_marker_runs() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault(VAULT_WIDE_ZONE_MARKER)),
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new("c", "turbovault:vector_reindex", serde_json::json!({}));
    let result = rt
        .invoke(&call)
        .await
        .expect("the marker grant authorises the rebuild");
    assert_eq!(result, "ok");
}

/// Path hardening in the resolver: `..` segments refuse, even in a `from` arg.
#[tokio::test]
async fn move_note_with_parent_segments_in_destination_is_refused() {
    let proposals = TempDir::new().unwrap();
    let caps = CapabilitySet::from_iter([
        Capability::ExecuteMcp("turbovault".into()),
        Capability::Write(Zone::vault("Tasks")),
        Capability::Write(Zone::vault("sandbox")),
    ]);
    let rt = runtime_for_grant(caps, proposals.path(), true);
    let call = ToolInvocation::new(
        "c",
        "turbovault:move_note",
        serde_json::json!({"from": "Tasks/Main.md", "to": "../escape.md"}),
    );
    let err = rt.invoke(&call).await.expect_err(
        "`..` in the destination must refuse (would silently resolve to a different zone)",
    );
    assert!(err.contains("not authorized"), "{err}");
    assert!(
        err.contains("parent-directory") || err.contains("escape"),
        "refusal must explain what we rejected: {err}"
    );
}
