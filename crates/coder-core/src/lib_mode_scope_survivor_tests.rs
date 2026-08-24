//! Split from `lib.rs` for module-health boundaries.

//! Survivor tests for CodingMode, DispatchWriteScope, PathPolicy defaults,
//! SessionReview, and render_findings_markdown. Each assertion was verified to
//! fail under the mutant it targets.

use crate::*;
use liberado_common::Outcome;
use serde_json::json;

// ── CodingMode ───────────────────────────────────────────────────────────

#[test]
fn from_payload_reads_every_wire_spelling_and_rejects_unknowns() {
    assert_eq!(
        CodingMode::from_payload(&json!({"mode": "plan"})),
        Some(CodingMode::Plan)
    );
    assert_eq!(
        CodingMode::from_payload(&json!({"mode": "EXPLORE"})),
        Some(CodingMode::Explore)
    );
    assert_eq!(
        CodingMode::from_payload(&json!({"mode": "normal"})),
        Some(CodingMode::Normal)
    );
    assert_eq!(
        CodingMode::from_payload(&json!({"explore_mode": true})),
        Some(CodingMode::Explore)
    );
    assert_eq!(
        CodingMode::from_payload(&json!({"plan_mode": true})),
        Some(CodingMode::Plan)
    );
    // Explore wins when both legacy booleans are set.
    assert_eq!(
        CodingMode::from_payload(&json!({"plan_mode": true, "explore_mode": true})),
        Some(CodingMode::Explore)
    );
    assert_eq!(CodingMode::from_payload(&json!({})), None);
    assert_eq!(CodingMode::from_payload(&json!({"mode": "yolo"})), None);
    assert_eq!(CodingMode::from_payload(&json!({"plan_mode": false})), None);
}

#[test]
fn each_mode_carries_its_distinct_policies_and_prompt() {
    // Each mode carries exactly its preset: a body replaced with Default::default()
    // would make every tier equal to Normal's policy.
    assert_eq!(CodingMode::Normal.path_policy(), PathPolicy::default());
    assert_eq!(CodingMode::Plan.path_policy(), PathPolicy::plan_mode());
    assert_eq!(CodingMode::Explore.path_policy(), PathPolicy::read_only());
    assert_ne!(PathPolicy::default(), PathPolicy::plan_mode());
    assert_ne!(PathPolicy::plan_mode(), PathPolicy::read_only());

    assert_eq!(
        CodingMode::Normal.command_policy(),
        CommandPolicy::default()
    );
    assert_eq!(
        CodingMode::Plan.command_policy(),
        CommandPolicy::none_allowed()
    );
    assert_eq!(
        CodingMode::Explore.command_policy(),
        CommandPolicy::none_allowed()
    );
    assert_ne!(CommandPolicy::default(), CommandPolicy::none_allowed());

    assert_eq!(CodingMode::Normal.coder_prompt(), None);
    assert_eq!(
        CodingMode::Plan.coder_prompt(),
        Some(PLAN_MODE_CODER_PROMPT)
    );
    assert_eq!(
        CodingMode::Explore.coder_prompt(),
        Some(EXPLORE_MODE_CODER_PROMPT)
    );

    assert!(!CodingMode::Normal.is_restricted());
    assert!(CodingMode::Plan.is_restricted());
    assert!(CodingMode::Explore.is_restricted());
}

#[test]
fn strictest_always_picks_the_more_restricted_tier() {
    use CodingMode::{Explore, Normal, Plan};
    assert_eq!(strictest_of(Normal, Plan), Plan);
    assert_eq!(strictest_of(Plan, Normal), Plan);
    assert_eq!(strictest_of(Plan, Explore), Explore);
    assert_eq!(strictest_of(Explore, Plan), Explore);
    assert_eq!(strictest_of(Normal, Normal), Normal);
    assert_eq!(strictest_of(Explore, Explore), Explore);

    fn strictest_of(a: CodingMode, b: CodingMode) -> CodingMode {
        CodingMode::strictest(a, b)
    }
}

// ── DispatchWriteScope ───────────────────────────────────────────────────

#[test]
fn write_scope_activity_and_permit_semantics() {
    let empty = DispatchWriteScope::default();
    assert!(!empty.is_active(), "no globs = no scope change");

    let deny_only = DispatchWriteScope {
        deny_globs: vec!["secrets/**".into()],
        ..DispatchWriteScope::default()
    };
    assert!(deny_only.is_active());
    assert!(
        !deny_only.permits("secrets/key.txt"),
        "deny list refuses matches"
    );
    assert!(
        deny_only.permits("src/main.rs"),
        "deny-only permits everything else"
    );

    let allow_only = DispatchWriteScope {
        allow_globs: vec!["docs/**".into()],
        ..DispatchWriteScope::default()
    };
    assert!(allow_only.is_active());
    assert!(allow_only.permits("docs/roadmap.md"));
    assert!(
        !allow_only.permits("src/main.rs"),
        "allow list is exclusive"
    );

    // `**` permits the whole workspace.
    let all = DispatchWriteScope {
        allow_globs: vec!["**".into()],
        ..DispatchWriteScope::default()
    };
    assert!(all.permits("deeply/nested/file.rs"));

    // Directory-prefix form and backslash normalization.
    let prefix = DispatchWriteScope {
        allow_globs: vec!["crates/**".into()],
        ..DispatchWriteScope::default()
    };
    assert!(prefix.permits("crates/foo/lib.rs"));
    assert!(
        prefix.permits(r"crates\foo\lib.rs"),
        "backslashes normalize to slashes"
    );
    assert!(!prefix.permits("crates_other/x"));
}

// ── PathPolicy defaults ──────────────────────────────────────────────────

#[test]
fn path_policy_defaults_deny_infrastructure_and_size_outputs() {
    let p = PathPolicy::default();
    assert_eq!(
        p.deny_globs,
        vec![
            ".git/**".to_string(),
            "target/**".to_string(),
            "node_modules/**".to_string()
        ]
    );
    assert_eq!(p.read_max_bytes, 128 * 1024);
    assert_eq!(p.search_max_results, 200);
    assert!(p.write_scope.allow_globs.is_empty() && p.write_scope.deny_globs.is_empty());

    // The fuzzy threshold default comes from EditConfig, single source of truth.
    let cfg = EditConfig::default();
    assert_eq!(cfg.fuzzy_threshold, EditConfig::DEFAULT_FUZZY_THRESHOLD);
}

// ── SessionReview ────────────────────────────────────────────────────────

/// `include_tool_names` defaults to TRUE when omitted from config — the serde default
/// function, not the bool's own Default. Dropping tool names measurably produced a
/// false accusation, so the omission must keep them on.
#[test]
fn critic_config_keeps_tool_names_by_default_when_omitted() {
    let cfg: SessionCriticConfig =
        serde_json::from_value(json!({"enabled": true})).expect("minimal critic config");
    assert!(cfg.include_tool_names);
    // And an explicit false still wins over the default.
    let cfg: SessionCriticConfig = serde_json::from_value(json!({
        "enabled": true,
        "include_tool_names": false
    }))
    .unwrap();
    assert!(!cfg.include_tool_names);
}

#[test]
fn findings_closed_section_requires_at_least_one_fixed_issue() {
    // Outstanding AND fixed together: the closed count is len - outstanding,
    // and the Closed section appears only when that difference exceeds zero.
    let r = result_with(
        vec![
            finding("open one", Disposition::Outstanding),
            finding("done one", Disposition::Fixed),
        ],
        vec![],
        None,
    );
    let md = render_findings_markdown(&r);
    assert!(md.contains("### Open — from the diff review"), "{md}");
    assert!(md.contains("### Closed"), "{md}");
    assert!(md.contains("1 diff-review issue(s)"), "{md}");

    // Only outstanding: no Closed section, no zero-count line.
    let r2 = result_with(
        vec![finding("only open", Disposition::Outstanding)],
        vec![],
        None,
    );
    let md2 = render_findings_markdown(&r2);
    assert!(md2.contains("not addressed"), "{md2}");
    assert!(
        !md2.contains("Closed"),
        "no fixed issues, no Closed section: {md2}"
    );
}

#[test]
fn session_review_is_clean_iff_no_findings() {
    let clean = SessionReview::default();
    assert!(clean.is_clean());

    let dirty = SessionReview {
        findings: vec![SessionFinding {
            kind: "unsupported_claim".into(),
            quote: "tests pass".into(),
            why: "they do not".into(),
            remedy: Remedy::Verify,
        }],
    };
    assert!(!dirty.is_clean());
}

// ── render_findings_markdown ─────────────────────────────────────────────

fn result_with(
    diff: Vec<DiffFinding>,
    session: Vec<SessionFinding>,
    remediation: Option<RemediationRecord>,
) -> CoderRunResult {
    serde_json::from_value(json!({
        "backend": "b",
        "outcome": Outcome::Failed,
        "summary": "s",
        "diff_findings": diff,
        "session_findings": session,
        "remediation": remediation,
    }))
    .expect("result fixture")
}

fn finding(issue: &str, d: Disposition) -> DiffFinding {
    DiffFinding {
        issue: issue.into(),
        disposition: d,
        first_seen_attempt: 0,
    }
}

#[test]
fn findings_render_orders_outstanding_session_and_closed() {
    let r = result_with(
        vec![finding("null deref", Disposition::Outstanding)],
        vec![SessionFinding {
            kind: "silent_reversal".into(),
            quote: "all good now".into(),
            why: "the test still fails".into(),
            remedy: Remedy::Repair,
        }],
        Some(RemediationRecord {
            branch: "fix/speculatively".into(),
            outcome: Outcome::Failed,
            summary: "tried".into(),
            addressed: Vec::new(),
        }),
    );
    let md = render_findings_markdown(&r);
    assert!(md.starts_with("## Review findings\n"), "{md}");
    let open_diff = md.find("Open — from the diff review").unwrap();
    let disputed = md.find("not addressed").unwrap();
    let session = md.find("Open — from the session review").unwrap();
    let spec = md.find("A speculative fix exists").unwrap();
    assert!(
        open_diff < disputed && disputed < session && session < spec,
        "{md}"
    );
    assert!(md.contains("**silent_reversal** (Repair) — the test still fails"));
    assert!(md.contains("> all good now"));
    assert!(md.contains("Branch `fix/speculatively`"));

    // A disputed finding gets its own label.
    let r2 = result_with(
        vec![finding("wrong claim", Disposition::Disputed)],
        vec![],
        None,
    );
    let md2 = render_findings_markdown(&r2);
    assert!(md2.contains("disputed by the implementer"), "{md2}");

    // Closed issues go last and are counted.
    let r3 = result_with(
        vec![finding("fixed thing", Disposition::Fixed)],
        vec![],
        None,
    );
    let md3 = render_findings_markdown(&r3);
    assert!(md3.contains("### Closed"), "{md3}");
    assert!(md3.contains("1 diff-review issue(s)"), "{md3}");

    // Nothing at all renders as empty so callers can append unconditionally.
    let r4 = result_with(vec![], vec![], None);
    assert_eq!(render_findings_markdown(&r4), "");
}
