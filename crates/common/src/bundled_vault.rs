//! Built-in TurboVault classifications.
//!
//! The live node's `topology.toml` still carries the pre-fix block: `zone_from_arg = "path"`,
//! the old 8-tool `write_tools` (including the stale names `update_task` and `delete_task`),
//! and no read, vault-wide, admin, or path-arg lists. [`apply_bundled_classifications`] fills
//! that gap for an MCP named `turbovault`. The operator's config can only extend or tighten
//! a class. A tool the operator lists as a write stays a write. A tool the defaults mark
//! admin stays admin even when the config still lists it as vault-wide. A name the defaults
//! do not know, such as `update_task`, keeps the operator's class and is otherwise harmless.
//!
//! Undeclared tools — names in neither the config nor these defaults — stay undeclared and
//! fail closed at the runtime gate.

use std::collections::BTreeMap;

use crate::catalog::McpDescriptor;

/// MCP names that receive [`apply_bundled_classifications`].
///
/// Detection is the MCP `name`, not the path-addressed shape. Another vault MCP that sets
/// `zone_from_arg` does not inherit TurboVault's tool list.
pub const BUNDLED_VAULT_MCP_NAMES: &[&str] = &["turbovault"];

/// One bundled MCP's default classes. Config unions into these and may only move a tool
/// to a stricter class.
#[derive(Debug, Clone, Copy)]
pub struct BundledClassification<'a> {
    pub write_tools: &'a [&'a str],
    pub read_tools: &'a [&'a str],
    pub vault_wide_writes: &'a [&'a str],
    pub admin_tools: &'a [&'a str],
    /// `(tool, [arg, ...])`. Operator entries win on the same tool name.
    pub write_path_args: &'a [(&'a str, &'a [&'a str])],
}

const TURBOVAULT_WRITE_TOOLS: &[&str] = &[
    "write_note",
    "edit_note",
    "delete_note",
    "move_note",
    "move_file",
    "update_frontmatter",
    "manage_tags",
    "create_from_template",
    "tasks_complete",
    "tasks_update",
    "tasks_delete",
    "generate_index",
    "append_log_entry",
    "visualize",
];

const TURBOVAULT_READ_TOOLS: &[&str] = &[
    "get_vault_context",
    "list_vaults",
    "get_active_vault",
    "get_vault_config",
    "read_note",
    "get_notes_info",
    "diff_notes",
    "search",
    "advanced_search",
    "recommend_related",
    "semantic_search",
    "find_duplicates",
    "find_stale_notes",
    "query_frontmatter_sql",
    "query_metadata",
    "get_metadata_value",
    "get_backlinks",
    "get_forward_links",
    "get_related_notes",
    "get_hub_notes",
    "get_dead_end_notes",
    "get_isolated_clusters",
    "get_centrality_ranking",
    "detect_cycles",
    "suggest_links",
    "quick_health_check",
    "full_health_analysis",
    "get_broken_links",
    "evaluate_note_quality",
    "vault_quality_report",
    "analyze_note_grounding",
    "find_ungrounded_notes",
    "audit_stats",
    "list_templates",
    "get_template",
    "export_health_report",
    "export_broken_links",
    "export_vault_stats",
    "export_analysis_report",
    "okf_validate",
    "tasks_list",
    "tasks_overdue",
    "tasks_tags",
    "tasks_config",
    "list_orphan_fanouts",
    "vector_search",
    "vector_status",
    "vector_config",
];

const TURBOVAULT_VAULT_WIDE_WRITES: &[&str] = &[
    "begin_fanout",
    "commit_fanout",
    "abandon_fanout",
    "vector_reindex",
];

const TURBOVAULT_ADMIN_TOOLS: &[&str] = &["set_active_vault"];

const TURBOVAULT_DEFAULT_WRITE_PATH_ARGS: &[(&str, &[&str])] = &[
    ("move_note", &["from", "to"]),
    ("move_file", &["from", "to"]),
    ("create_from_template", &["file_path"]),
    ("generate_index", &["directory"]),
    ("append_log_entry", &["directory"]),
    ("visualize", &["output"]),
];

/// Defaults for a bundled MCP name. `None` when the name is not bundled.
pub fn default_bundled_classification(name: &str) -> Option<BundledClassification<'_>> {
    if name != "turbovault" {
        return None;
    }
    Some(BundledClassification {
        write_tools: TURBOVAULT_WRITE_TOOLS,
        read_tools: TURBOVAULT_READ_TOOLS,
        vault_wide_writes: TURBOVAULT_VAULT_WIDE_WRITES,
        admin_tools: TURBOVAULT_ADMIN_TOOLS,
        write_path_args: TURBOVAULT_DEFAULT_WRITE_PATH_ARGS,
    })
}

/// True when `name` is a bundled vault MCP.
pub fn is_bundled_mcp(name: &str) -> bool {
    BUNDLED_VAULT_MCP_NAMES.contains(&name)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolClass {
    Read = 1,
    ZoneWrite = 2,
    VaultWide = 3,
    Admin = 4,
}

/// Fill gaps in a bundled MCP's classification. Non-bundled descriptors are unchanged.
///
/// For each tool name, the class is the stricter of the operator's class and the default.
/// Strictness is read < zone write < vault-wide < admin. A stale operator name that the
/// defaults do not mention keeps the operator's class.
pub fn apply_bundled_classifications(mut descriptor: McpDescriptor) -> McpDescriptor {
    let Some(bundled) = default_bundled_classification(&descriptor.name) else {
        return descriptor;
    };
    let names = tool_names(&descriptor, bundled);
    let mut write = Vec::new();
    let mut read = Vec::new();
    let mut vault_wide = Vec::new();
    let mut admin = Vec::new();
    for name in names {
        let class = stricter(
            operator_class(&descriptor, &name),
            default_class(bundled, &name),
        );
        match class {
            Some(ToolClass::ZoneWrite) => write.push(name),
            Some(ToolClass::Read) => read.push(name),
            Some(ToolClass::VaultWide) => vault_wide.push(name),
            Some(ToolClass::Admin) => admin.push(name),
            None => {}
        }
    }
    descriptor.write_tools = write;
    descriptor.read_tools = read;
    descriptor.vault_wide_writes = vault_wide;
    descriptor.admin_tools = admin;
    descriptor.write_path_args = merge_path_args(&descriptor.write_path_args, bundled);
    descriptor
}

fn tool_names(descriptor: &McpDescriptor, bundled: BundledClassification<'_>) -> Vec<String> {
    let mut names = Vec::new();
    for tool in descriptor
        .write_tools
        .iter()
        .chain(&descriptor.read_tools)
        .chain(&descriptor.vault_wide_writes)
        .chain(&descriptor.admin_tools)
    {
        push_name(&mut names, tool.clone());
    }
    for tool in bundled
        .write_tools
        .iter()
        .chain(bundled.read_tools)
        .chain(bundled.vault_wide_writes)
        .chain(bundled.admin_tools)
    {
        push_name(&mut names, (*tool).to_string());
    }
    names
}

fn push_name(names: &mut Vec<String>, tool: String) {
    if !names.iter().any(|existing| existing == &tool) {
        names.push(tool);
    }
}

fn operator_class(descriptor: &McpDescriptor, tool: &str) -> Option<ToolClass> {
    stricter(
        class_if(&descriptor.admin_tools, tool, ToolClass::Admin),
        stricter(
            class_if(&descriptor.vault_wide_writes, tool, ToolClass::VaultWide),
            stricter(
                class_if(&descriptor.write_tools, tool, ToolClass::ZoneWrite),
                class_if(&descriptor.read_tools, tool, ToolClass::Read),
            ),
        ),
    )
}

fn default_class(bundled: BundledClassification<'_>, tool: &str) -> Option<ToolClass> {
    stricter(
        class_if_str(bundled.admin_tools, tool, ToolClass::Admin),
        stricter(
            class_if_str(bundled.vault_wide_writes, tool, ToolClass::VaultWide),
            stricter(
                class_if_str(bundled.write_tools, tool, ToolClass::ZoneWrite),
                class_if_str(bundled.read_tools, tool, ToolClass::Read),
            ),
        ),
    )
}

fn class_if(tools: &[String], tool: &str, class: ToolClass) -> Option<ToolClass> {
    tools.iter().any(|name| name == tool).then_some(class)
}

fn class_if_str(tools: &[&str], tool: &str, class: ToolClass) -> Option<ToolClass> {
    tools.contains(&tool).then_some(class)
}

fn stricter(left: Option<ToolClass>, right: Option<ToolClass>) -> Option<ToolClass> {
    match (left, right) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

#[cfg(test)]
#[path = "bundled_vault_tests.rs"]
mod tests;

fn merge_path_args(
    operator: &BTreeMap<String, Vec<String>>,
    bundled: BundledClassification<'_>,
) -> BTreeMap<String, Vec<String>> {
    let mut merged = operator.clone();
    for (tool, args) in bundled.write_path_args {
        merged
            .entry((*tool).to_string())
            .or_insert_with(|| args.iter().map(|arg| (*arg).to_string()).collect());
    }
    merged
}
