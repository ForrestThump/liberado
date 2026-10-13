use super::*;
use crate::Consequence;
use crate::catalog::McpDescriptor;

/// The live node's old-shape block — legacy 8-tool `write_tools`, no read, vault-wide,
/// path-arg, or admin lists — classifies every bundled tool. `read_note` and
/// `vector_search` are reads. `edit_note`, `tasks_update`, and `manage_tags` are writes.
/// `vector_reindex` is vault-wide. `set_active_vault` is admin.
#[test]
fn livetv_with_bundled_defaults_returns_a_fully_classified_descriptor() {
    let minimal = McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        // The legacy 8-tool list as the live node has shipped it since 2026-10-11.
        write_tools: vec![
            "write_note".into(),
            "delete_note".into(),
            "move_note".into(),
            "move_file".into(),
            "update_frontmatter".into(),
            "create_from_template".into(),
            "update_task".into(),
            "delete_task".into(),
        ],
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    };
    let merged = apply_bundled_classifications(minimal);

    // The defaults filled the gaps:
    for tool in [
        "edit_note",
        "manage_tags",
        "tasks_complete",
        "tasks_update",
        "tasks_delete",
        "generate_index",
        "append_log_entry",
        "visualize",
    ] {
        assert!(
            merged.write_tools.contains(&tool.to_string()),
            "operator's legacy config is silently extended with defaults: '{tool}' must be in write_tools, got {merged:?}"
        );
    }
    for tool in [
        "read_note",
        "search",
        "tasks_list",
        "vector_search",
        "vector_status",
        "vector_config",
    ] {
        assert!(
            merged.read_tools.contains(&tool.to_string()),
            "defaults must classify '{tool}' as a read, got {merged:?}"
        );
    }
    // The legacy operator list still gets UNIONED, never demoted.
    assert!(merged.write_tools.contains(&"write_note".to_string()));
    // Stale names (update_task, delete_task) are harmless — the live server has no such
    // tool today, so they sit in the operator's write_tools without runtime effect.
    assert!(merged.write_tools.contains(&"update_task".to_string()));

    // Multi-arg writer's path args are filled by the defaults.
    for tool in ["move_note", "move_file"] {
        assert!(
            merged.write_path_args.contains_key(tool),
            "multi-arg writer '{tool}' missing from defaults' write_path_args: {merged:?}"
        );
    }
    assert_eq!(
        merged.write_path_args.get("move_note").unwrap(),
        &vec!["from".to_string(), "to".to_string()]
    );

    // `set_active_vault` becomes admin, never a vault-wide write.
    assert!(merged.admin_tools.contains(&"set_active_vault".to_string()));
    assert!(
        !merged
            .vault_wide_writes
            .contains(&"set_active_vault".to_string())
    );
}

/// Bundled merge — the operator's config can EXTEND but never DEMOTE. Adding `edit_note`
/// again (it's already in the defaults) is a no-op; declaring it explicitly as a read while
/// the defaults say write keeps it a write.
#[test]
fn bundled_merge_operator_declared_write_is_never_demoted_to_read() {
    let minimal = McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        // Operator's explicit — might be wrong but the runtime respects it.
        write_tools: vec!["write_note".into(), "edit_note".into()],
        read_tools: vec!["edit_note".into()], // Operator says edit_note is a read
        vault_wide_writes: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    };
    let merged = apply_bundled_classifications(minimal);
    // The defaults would have set edit_note as a write; the operator says it's a read, but
    // union-never-demote means `edit_note` STAYS in write_tools. The `read_tools` entry is
    // dropped because the run classifies it as a write.
    assert!(
        merged.write_tools.contains(&"edit_note".to_string()),
        "operator's write classification must take precedence over the defaults' read: {merged:?}"
    );
    assert!(
        !merged.read_tools.contains(&"edit_note".to_string()),
        "the merged descriptor must not list edit_note as both read and write: {merged:?}"
    );
}

/// Bundled merge — non-bundled MCP names pass through unchanged.
#[test]
fn bundled_merge_is_a_noop_for_non_bundled_mcps() {
    let minimal = McpDescriptor {
        name: "liberado-pdf-mcp".into(),
        description: "pdf".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: None,
        write_tools: Vec::new(),
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    };
    let merged = apply_bundled_classifications(minimal.clone());
    // No change — bundled defaults don't apply.
    assert_eq!(merged.write_tools, minimal.write_tools);
    assert_eq!(merged.read_tools, minimal.read_tools);
    assert_eq!(merged.admin_tools, minimal.admin_tools);
}

/// Listing a default write only as a read does not demote it. Config can tighten, not loosen.
#[test]
fn bundled_merge_does_not_demote_a_default_write_listed_only_as_a_read() {
    let minimal = McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: Vec::new(),
        read_tools: vec!["edit_note".into()],
        vault_wide_writes: vec!["set_active_vault".into()],
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    };
    let merged = apply_bundled_classifications(minimal);
    assert!(merged.write_tools.iter().any(|tool| tool == "edit_note"));
    assert!(!merged.read_tools.iter().any(|tool| tool == "edit_note"));
    assert!(
        merged
            .admin_tools
            .iter()
            .any(|tool| tool == "set_active_vault")
    );
    assert!(
        !merged
            .vault_wide_writes
            .iter()
            .any(|tool| tool == "set_active_vault")
    );
}

/// Every tool the live TurboVault server exposes lands in exactly one class.
#[test]
fn bundled_defaults_classify_each_live_tool_once() {
    let minimal = McpDescriptor {
        name: "turbovault".into(),
        description: "live turbovault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        zone_from_arg: Some("path".into()),
        ..Default::default()
    };
    let merged = apply_bundled_classifications(minimal);
    let live = [
        "get_vault_context",
        "read_note",
        "write_note",
        "edit_note",
        "delete_note",
        "move_note",
        "get_backlinks",
        "get_forward_links",
        "get_related_notes",
        "get_hub_notes",
        "get_dead_end_notes",
        "get_isolated_clusters",
        "quick_health_check",
        "full_health_analysis",
        "get_broken_links",
        "detect_cycles",
        "search",
        "advanced_search",
        "recommend_related",
        "query_frontmatter_sql",
        "list_templates",
        "get_template",
        "create_from_template",
        "list_vaults",
        "get_vault_config",
        "set_active_vault",
        "get_active_vault",
        "begin_fanout",
        "commit_fanout",
        "abandon_fanout",
        "list_orphan_fanouts",
        "export_health_report",
        "export_broken_links",
        "export_vault_stats",
        "export_analysis_report",
        "query_metadata",
        "get_metadata_value",
        "update_frontmatter",
        "manage_tags",
        "get_notes_info",
        "move_file",
        "suggest_links",
        "get_centrality_ranking",
        "diff_notes",
        "evaluate_note_quality",
        "vault_quality_report",
        "analyze_note_grounding",
        "find_ungrounded_notes",
        "okf_validate",
        "generate_index",
        "append_log_entry",
        "visualize",
        "find_stale_notes",
        "semantic_search",
        "find_duplicates",
        "audit_stats",
        "vector_search",
        "vector_reindex",
        "vector_status",
        "vector_config",
        "tasks_list",
        "tasks_overdue",
        "tasks_tags",
        "tasks_complete",
        "tasks_update",
        "tasks_delete",
        "tasks_config",
    ];
    let mut seen = Vec::new();
    for tool in merged
        .write_tools
        .iter()
        .chain(&merged.read_tools)
        .chain(&merged.vault_wide_writes)
        .chain(&merged.admin_tools)
    {
        assert!(
            !seen.contains(tool),
            "tool '{tool}' is in more than one class"
        );
        seen.push(tool.clone());
    }
    assert_eq!(seen.len(), live.len());
    for tool in live {
        assert!(
            seen.iter().any(|name| name == tool),
            "live tool '{tool}' has no bundled class"
        );
    }
}
