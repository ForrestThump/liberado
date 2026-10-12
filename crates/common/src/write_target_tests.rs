use super::*;
use crate::catalog::McpDescriptor;
use crate::{Consequence, WriteClass};

fn vault_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "vault".into(),
        description: "git-tracked vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: Some("tasks".into()),
        tool_zones: vec![("write_review".into(), Some("reviews".into()))],
        zone_from_arg: None,
        write_tools: Vec::new(),
        ..Default::default()
    }
}

/// A path-addressed MCP: one write tool that can land in any zone, plus a read that must NOT
/// be made to look like a write just because it also carries a path.
///
/// `read_note` is now declared in `read_tools` — pre-2026-10-11 the unlisted-read path
/// returned `NotAWrite` directly, which is the correct answer for THIS descriptor shape (it
/// has a `read_tools` list and `read_note` is in it). Tests that exercise the *undeclared*
/// path now build their own descriptor or call `write_target` on a tool name no list covers.
fn path_addressed_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "turbovault".into(),
        description: "path-addressed vault".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into(), "delete_note".into()],
        read_tools: vec!["read_note".into()],
        ..Default::default()
    }
}

#[test]
fn a_path_addressed_write_takes_its_zone_from_the_argument() {
    // The whole reason `write_target` exists: `default_zone` cannot describe this MCP. The SAME
    // tool writes to a different zone on every call, so a fixed declaration would authorize a
    // write to `decisions/` under a `Write(tasks)` capability.
    let d = path_addressed_descriptor();
    assert_eq!(
        write_target(
            &d,
            "write_note",
            &serde_json::json!({"path": "decisions/x.md"})
        ),
        WriteTarget::Zone("decisions".into())
    );
    assert_eq!(
        write_target(&d, "write_note", &serde_json::json!({"path": "tasks/y.md"})),
        WriteTarget::Zone("tasks".into())
    );
}

#[test]
fn a_read_on_a_path_addressed_mcp_is_not_a_write() {
    // `read_note` also carries a path. Without `write_tools`, it would demand a Write capability.
    let d = path_addressed_descriptor();
    assert_eq!(
        write_target(
            &d,
            "read_note",
            &serde_json::json!({"path": "finance/secret.md"})
        ),
        WriteTarget::NotAWrite
    );
}

#[test]
fn a_write_whose_zone_cannot_be_determined_fails_closed() {
    // A write we cannot place is a write we cannot authorize. Collapsing this into "not a write"
    // is exactly the bug F1 was — a guard that says nothing when it does not know.
    let d = path_addressed_descriptor();
    assert!(matches!(
        write_target(&d, "write_note", &serde_json::json!({})),
        WriteTarget::Undeterminable(_)
    ));
    assert!(
        matches!(
            write_target(&d, "write_note", &serde_json::json!({"path": "loose.md"})),
            WriteTarget::Undeterminable(_)
        ),
        "a bare filename names no zone (path hardening must keep this refusal)"
    );
}

// The 2026-10-11 live bug pinned here. This is the regression test for the hole:
// an undeclared tool on a path-addressed MCP was silently treated as a read, so an
// `edit_note` call (added to turbovault since the deployed topology was written) sailed
// through every gate under any grant that held `ExecuteMcp("turbovault")`. The new rule
// fails closed, naming the MCP, the tool, and the three lists the operator must use to
// classify it.
#[test]
fn an_undeclared_tool_on_a_path_addressed_mcp_fails_closed_with_a_remediation_hint() {
    let d = path_addressed_descriptor();
    let result = write_target(&d, "edit_note", &serde_json::json!({"path": "X/y.md"}));
    match result {
        WriteTarget::Undeterminable(why) => {
            assert!(
                why.contains("edit_note") && why.contains("turbovault"),
                "the refusal must name the tool and the MCP: {why}"
            );
            assert!(
                why.contains("write_tools")
                    && why.contains("read_tools")
                    && why.contains("vault_wide_writes"),
                "the refusal must point at the three classification lists: {why}"
            );
        }
        other => panic!("undeclared tool must be Undeterminable, got {other:?}"),
    }

    // And the no-arg shape: an undeclared writer called with NO path arg is also
    // Undeterminable, with the same actionable message.
    let no_path = write_target(&d, "edit_note", &serde_json::json!({}));
    assert!(
        matches!(no_path, WriteTarget::Undeterminable(_)),
        "an undeclared tool is undeclared regardless of its arguments: {no_path:?}"
    );
}

#[test]
fn a_vault_wide_write_resolves_to_vaultwide_not_zone() {
    // `begin_fanout` mutates git/fanout state, not a single note. There is no zone to resolve
    // from arguments, and `VaultWide` is the third class of path-addressed writer.
    let mut d = path_addressed_descriptor();
    d.vault_wide_writes = vec!["begin_fanout".into(), "commit_fanout".into()];
    let result = write_target(&d, "begin_fanout", &serde_json::json!({}));
    assert_eq!(
        result,
        WriteTarget::VaultWide,
        "a fanout mutator is VaultWide, not Zone or Undeterminable: {result:?}"
    );
    assert_eq!(
        write_target(&d, "commit_fanout", &serde_json::json!({})),
        WriteTarget::VaultWide,
    );
    // Sanity: `write_note` on the same MCP still resolves its zone from the path arg, because
    // the three classifications do NOT swallow the original write-tool resolution.
    assert_eq!(
        write_target(&d, "write_note", &serde_json::json!({"path": "tasks/x.md"})),
        WriteTarget::Zone("tasks".into())
    );
}

#[test]
fn a_tool_listed_explicitly_as_read_is_not_a_write() {
    // `read_tools` is the explicit read declaration. A tool named there is `NotAWrite` even
    // when the same descriptor has `write_tools` — the lists are independent.
    let d = path_addressed_descriptor();
    assert_eq!(
        write_target(&d, "read_note", &serde_json::json!({"path": "X/y.md"})),
        WriteTarget::NotAWrite,
    );
}

#[test]
fn fixed_zone_mcp_unchanged_an_unlisted_tool_is_not_a_write() {
    // Fixed-zone MCPs retain the original behavior: a tool with no zone mapping (no entry in
    // `tool_zones`, no `default_zone` to inherit) resolves to `NotAWrite` rather than
    // `Undeterminable`. This is a deliberate carve-out because fixed-zone MCPs use
    // `default_zone` for an MCP-wide declaration, not a deny-list — a fixed-zone MCP that
    // hasn't opted a tool into zone tracking is "this tool does not write a vault zone."
    let d = McpDescriptor {
        name: "tasks-mcp".into(),
        description: "task ops".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None, // explicitly: this MCP has no zone tracking
        ..Default::default()
    };
    assert_eq!(
        write_target(&d, "anything", &serde_json::json!({})),
        WriteTarget::NotAWrite,
    );
}

// The proptest invariant: a path-addressed MCP's classification is exhaustive (every tool in
// exactly one of write_tools/read_tools/vault_wide_writes classifies; unclassified is
// `Undeterminable`). This is what makes the 2026-10-11 bug structurally impossible: an upgrade
// that adds a new tool to turbovault without updating the operator's topology.toml surfaces
// as `Undeterminable` at the gate, never as `NotAWrite`.
#[test]
fn a_path_addressed_classification_is_exhaustive() {
    let d = path_addressed_descriptor();
    // Every tool in write_tools: resolves to Zone or Undeterminable (depending on path arg).
    for tool in &d.write_tools {
        let r = write_target(&d, tool, &serde_json::json!({"path": "tasks/x.md"}));
        assert!(
            matches!(r, WriteTarget::Zone(_)),
            "writer {tool} with path arg must be Zone: {r:?}"
        );
    }
    // Every tool in read_tools: NotAWrite, regardless of arguments.
    for tool in &d.read_tools {
        let r = write_target(&d, tool, &serde_json::json!({"path": "tasks/x.md"}));
        assert!(
            matches!(r, WriteTarget::NotAWrite),
            "reader {tool} with path arg must be NotAWrite: {r:?}"
        );
    }
    // Every tool in vault_wide_writes: VaultWide, regardless of arguments.
    for tool in &d.vault_wide_writes {
        let r = write_target(&d, tool, &serde_json::json!({}));
        assert!(
            matches!(r, WriteTarget::VaultWide),
            "vault-wide writer {tool} must be VaultWide: {r:?}"
        );
    }
    // A tool in NONE of the three: Undeterminable (fail closed).
    let r = write_target(
        &d,
        "brand_new_tool_added_in_a_future_release",
        &serde_json::json!({}),
    );
    assert!(
        matches!(r, WriteTarget::Undeterminable(_)),
        "an undeclared tool must be Undeterminable: {r:?}"
    );
}

#[test]
fn restricted_zone_is_flagged() {
    let restriction = zone_write_restriction(
        "vault",
        "write_review",
        &[vault_descriptor()],
        &[("reviews".to_string(), WriteClass::ProposalOnly)],
    );
    assert_eq!(restriction, Some("reviews".to_string()));
}

#[test]
fn agent_writable_zone_is_not_restricted() {
    let restriction = zone_write_restriction(
        "vault",
        "write_review",
        &[vault_descriptor()],
        &[("reviews".to_string(), WriteClass::AgentWritable)],
    );
    assert_eq!(restriction, None);
}

#[test]
fn unlisted_zone_fails_safe_to_restricted() {
    // "reviews" isn't in zone_write_classes at all — must fail safe (ProposalOnly default),
    // not silently pass just because nothing was configured for it.
    let restriction = zone_write_restriction(
        "vault",
        "write_review",
        &[vault_descriptor()],
        &[("tasks".to_string(), WriteClass::AgentWritable)],
    );
    assert_eq!(restriction, Some("reviews".to_string()));
}

#[test]
fn mcp_not_in_catalog_is_not_restricted() {
    // The capability guard (checked separately by each caller) already rejects an ungranted
    // MCP — this function isn't the place that catches that.
    let restriction = zone_write_restriction(
        "some-other-mcp",
        "anything",
        &[vault_descriptor()],
        &[("reviews".to_string(), WriteClass::ProposalOnly)],
    );
    assert_eq!(restriction, None);
}

#[test]
fn tool_not_opted_into_zone_tracking_is_not_restricted() {
    // "tasks-mcp" has no default_zone/tool_zones at all, so resolve_zone returns None
    // regardless of what's restricted elsewhere.
    let untracked = McpDescriptor {
        name: "tasks-mcp".into(),
        description: "task ops".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        ..Default::default()
    };
    let restriction = zone_write_restriction(
        "tasks-mcp",
        "add",
        &[untracked],
        &[("reviews".to_string(), WriteClass::ProposalOnly)],
    );
    assert_eq!(restriction, None);
}

#[test]

fn write_target_empty_segment_is_not_a_zone() {
    let d = path_addressed_descriptor();
    // In the MIDDLE of a path, empty segments must be skipped. `a//b.md` should resolve
    // to zone `a`. (Leading `//` is rejected as absolute — see the path-hardening tests
    // below — but middle empty segments are a normal-but-quirky input we tolerate.)
    // The correct `&&` skips empties to find "a". A `||` mutation would match the
    // empty string first and return `Zone("")` — a zone we cannot authorize.
    let result = write_target(&d, "write_note", &serde_json::json!({"path": "a//b.md"}));
    assert_eq!(
        result,
        WriteTarget::Zone("a".into()),
        "interior empty segments must be skipped to find the real zone: got {result:?}"
    );
}

fn hub_descriptor() -> McpDescriptor {
    McpDescriptor {
        name: "turbovault".into(),
        description: "test".into(),
        consequence: Consequence::Reversible,
        provenance: None,
        default_zone: None,
        tool_zones: Vec::new(),
        zone_from_arg: Some("path".into()),
        write_tools: vec!["write_note".into(), "edit_note".into()],
        read_tools: Vec::new(),
        vault_wide_writes: Vec::new(),
        write_path_args: std::collections::BTreeMap::new(),
        admin_tools: Vec::new(),
    }
}

/// An absolute path is `Undeterminable` ("refuse; vault writes must be relative paths").
#[test]
fn write_target_refuses_absolute_paths() {
    let d = hub_descriptor();
    for bad in [
        "/etc/passwd",
        "\\Windows\\System32",
        "C:/Windows/System32",
        "C:\\Users\\Admin",
        "/Tasks/x.md",
        "\\Tasks/x.md",
    ] {
        let result = write_target(&d, "write_note", &serde_json::json!({"path": bad}));
        assert!(
            matches!(result, WriteTarget::Undeterminable(_)),
            "absolute path '{bad}' must refuse, got {result:?}"
        );
    }
}

/// `..` segments refuse — `Tasks/../Legal/x.md` LOOKS like Tasks but resolves elsewhere.
#[test]
fn write_target_refuses_parent_directory_segments() {
    let d = hub_descriptor();
    let result = write_target(
        &d,
        "write_note",
        &serde_json::json!({"path": "Tasks/../Legal/x.md"}),
    );
    assert!(
        matches!(result, WriteTarget::Undeterminable(_)),
        "`..` segment must refuse (would silently resolve to a different zone): {result:?}"
    );
    // First-segment `..` is the most dangerous — `../etc/passwd` must also refuse, not
    // resolve to "etc".
    let result = write_target(
        &d,
        "write_note",
        &serde_json::json!({"path": "../../etc/passwd"}),
    );
    assert!(
        matches!(result, WriteTarget::Undeterminable(_)),
        "leading `..` must refuse: {result:?}"
    );
}

/// `.` segments normalise silently — `a/./b.md` collapses to `a/b.md`, `a//b.md`
/// collapses to `a/b.md`.
#[test]
fn write_target_normalises_dot_segments() {
    let d = hub_descriptor();
    assert_eq!(
        write_target(
            &d,
            "write_note",
            &serde_json::json!({"path": "Tasks/./Main.md"})
        ),
        WriteTarget::Zone("Tasks".into()),
    );
    assert_eq!(
        write_target(
            &d,
            "write_note",
            &serde_json::json!({"path": "Tasks//Main.md"})
        ),
        WriteTarget::Zone("Tasks".into()),
    );
}

/// Empty paths and bare filenames refuse — a write with no leading zone cannot be
/// authorised ("names no zone" branch already covers the unclassifiable case).
#[test]
fn write_target_refuses_empty_paths_and_bare_filenames() {
    let d = hub_descriptor();
    for bad in ["", "loose.md", "/", ".", "./"] {
        let result = write_target(&d, "write_note", &serde_json::json!({"path": bad}));
        // `.` and `./` normalise to empty; `loose.md` is just one segment without a zone;
        // `""` is empty; `/` has leading separator (absolute). All refuse.
        assert!(
            matches!(result, WriteTarget::Undeterminable(_)),
            "shape '{bad}' must refuse, got {result:?}"
        );
    }
}

/// NUL bytes and other control characters refuse — they are not legal in a vault path
/// and must not propagate to the filesystem layer.
#[test]
fn write_target_refuses_control_characters() {
    let d = hub_descriptor();
    for bad in ["Tasks/\u{0}foo.md", "Tasks/\nMain.md", "Tasks/\r\nMain.md"] {
        let result = write_target(&d, "write_note", &serde_json::json!({"path": bad}));
        assert!(
            matches!(result, WriteTarget::Undeterminable(_)),
            "control chars in '{bad:?}' must refuse, got {result:?}"
        );
    }
}

/// Zone names are **case-sensitive**. A path whose leading segment matches a granted zone
/// only case-insensitively ("tasks/x.md" with the grant holding `Write(Tasks)`) must NOT
/// be authorised; the operator must classify the case-insensitive variant explicitly.
#[test]
fn write_target_zone_match_is_case_sensitive() {
    let d = hub_descriptor();
    for bad in ["tasks/x.md", "TASKS/x.md", "TaSkS/x.md"] {
        let result = write_target(&d, "write_note", &serde_json::json!({"path": bad}));
        // The path-zone resolution is case-preserving — `tasks` is the zone the gate sees.
        // The grant/zone comparison that follows in `risk_gated.rs` is then exact-match,
        // so `Write(Tasks)` does NOT satisfy a write targeting `tasks`. This test pins the
        // resolution shape; the grant-comparison side is exercised in `risk_gated_tests`.
        assert_eq!(
            result,
            WriteTarget::Zone(bad.split('/').next().unwrap().to_string()),
            "the resolver preserves case for the zone segment ('{bad}' → the literal case)"
        );
    }
}

/// `..` in NON-leading segments still refuses. `a/b/../c.md` LOOKS like `a/b/`'s zone
/// (`a`) but resolves to `a/c.md`.
#[test]
fn write_target_refuses_trailing_parent_segments() {
    let d = hub_descriptor();
    let result = write_target(
        &d,
        "write_note",
        &serde_json::json!({"path": "Tasks/inner/../escape.md"}),
    );
    assert!(
        matches!(result, WriteTarget::Undeterminable(_)),
        "`..` anywhere in the path must refuse: {result:?}"
    );
}

/// Backslash variants of separators are normalised alongside `/`. `Tasks\\Main.md` is a
/// Linux→Windows port; the resolver should treat it identically to `Tasks/Main.md`.
#[test]
fn write_target_treats_backslash_and_forward_separators_identically() {
    let d = hub_descriptor();
    assert_eq!(
        write_target(
            &d,
            "write_note",
            &serde_json::json!({"path": "Tasks\\Main.md"})
        ),
        WriteTarget::Zone("Tasks".into()),
    );
}

/// Multi-arg writer (`move_note(from, to)`) — both segments are hardened, `..` in
/// either is a refusal.
#[test]
fn write_target_refuses_parent_in_secondary_path_arg() {
    let mut d = hub_descriptor();
    d.write_tools = vec!["move_note".into()];
    d.write_path_args
        .insert("move_note".into(), vec!["from".into(), "to".into()]);

    // Source is fine, destination has `..` — refuse.
    let result = write_target(
        &d,
        "move_note",
        &serde_json::json!({"from": "Tasks/Main.md", "to": "../escape.md"}),
    );
    assert!(
        matches!(result, WriteTarget::Undeterminable(_)),
        "`..` in the destination must refuse: {result:?}"
    );

    // Source has `..` — refuse.
    let result = write_target(
        &d,
        "move_note",
        &serde_json::json!({"from": "../escape.md", "to": "sandbox/x.md"}),
    );
    assert!(
        matches!(result, WriteTarget::Undeterminable(_)),
        "`..` in the source must refuse: {result:?}"
    );
}

/// An array-of-paths argument is checked element by element, including a `..` in any element.
#[test]
fn write_target_checks_every_element_of_a_path_array() {
    let mut d = hub_descriptor();
    d.write_tools = vec!["batch_move".into()];
    d.write_path_args
        .insert("batch_move".into(), vec!["paths".into()]);
    assert_eq!(
        write_target(
            &d,
            "batch_move",
            &serde_json::json!({"paths": ["Tasks/a.md", "Work/b.md"]}),
        ),
        WriteTarget::Zones(vec!["Tasks".into(), "Work".into()]),
    );
    assert!(matches!(
        write_target(
            &d,
            "batch_move",
            &serde_json::json!({"paths": ["Tasks/a.md", "Tasks/../Legal/x.md"]}),
        ),
        WriteTarget::Undeterminable(_)
    ));
}

/// Confirmation path arguments are checked when the call carries them.
#[test]
fn write_target_checks_confirm_path_arguments_when_present() {
    let mut d = hub_descriptor();
    d.write_tools = vec!["move_file".into()];
    d.write_path_args
        .insert("move_file".into(), vec!["from".into(), "to".into()]);
    assert_eq!(
        write_target(
            &d,
            "move_file",
            &serde_json::json!({
                "from": "Tasks/a.md",
                "to": "Tasks/b.md",
                "confirm_to": "Work/b.md",
            }),
        ),
        WriteTarget::Zones(vec!["Tasks".into(), "Work".into()]),
    );
}
