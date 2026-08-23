//! Split from `lib.rs` for module-health boundaries.

use super::*;
use std::path::Path;

fn result_with(diff: Vec<DiffFinding>, session: Vec<SessionFinding>) -> CoderRunResult {
    CoderRunResult {
        backend: "t".into(),
        outcome: Outcome::Succeeded,
        summary: "done".into(),
        files_changed: Vec::new(),
        file_changes: Vec::new(),
        validation_notes: None,
        critic_verdict: None,
        gate_votes: Vec::new(),
        trace_path: None,
        diff_findings: diff,
        session_findings: session,
        remediation: None,
        diagnostics: serde_json::Value::Null,
    }
}

fn diff(issue: &str, disposition: Disposition) -> DiffFinding {
    DiffFinding {
        issue: issue.into(),
        disposition,
        first_seen_attempt: 0,
    }
}

fn session(kind: &str, remedy: Remedy) -> SessionFinding {
    SessionFinding {
        kind: kind.into(),
        quote: "the mutation test passes even when I break run_headless".into(),
        why: "shipped it anyway".into(),
        remedy,
    }
}

/// Only `Repair` and `Verify` can be handed to a coding agent. Sending one to fix a paragraph
/// spends a whole run on a text edit.
#[test]
fn only_code_shaped_remedies_are_actionable() {
    assert!(Remedy::Repair.is_actionable());
    assert!(Remedy::Verify.is_actionable());
    assert!(!Remedy::Retract.is_actionable());
    assert!(!Remedy::None.is_actionable());
}

#[test]
fn actionable_filters_the_review() {
    let review = SessionReview {
        findings: vec![
            session("abandoned_finding", Remedy::Repair),
            session("unsupported_claim", Remedy::Retract),
            session("silent_reversal", Remedy::Verify),
        ],
    };
    let kinds: Vec<&str> = review
        .actionable()
        .iter()
        .map(|f| f.kind.as_str())
        .collect();
    assert_eq!(kinds, vec!["abandoned_finding", "silent_reversal"]);
}

/// Nothing to report must render as nothing, not as an empty heading a reader has to scan.
#[test]
fn a_clean_run_renders_empty() {
    assert!(render_findings_markdown(&result_with(Vec::new(), Vec::new())).is_empty());
}

/// The ordering *is* the mechanism. A run that fixed four issues and left one open must not
/// read as a clean run with a footnote — the open item is why anyone is reading.
#[test]
fn open_findings_come_before_closed_ones() {
    let rendered = render_findings_markdown(&result_with(
        vec![
            diff("cosmetic thing", Disposition::Fixed),
            diff("the test does not bind", Disposition::Outstanding),
        ],
        Vec::new(),
    ));
    let open = rendered
        .find("the test does not bind")
        .expect("open issue shown");
    let closed = rendered.find("### Closed").expect("closed section shown");
    assert!(
        open < closed,
        "an outstanding finding must not sit below the resolved ones:\n{rendered}"
    );
}

/// A resolved issue must not be presented as open. Crying wolf on fixed work is how a reader
/// learns to skip the section.
#[test]
fn a_fixed_issue_is_not_reported_as_open() {
    let rendered = render_findings_markdown(&result_with(
        vec![diff("gone now", Disposition::Fixed)],
        Vec::new(),
    ));
    let open_section = rendered.split("### Closed").next().unwrap_or("");
    assert!(
        !open_section.contains("gone now"),
        "a fixed issue appeared above the Closed heading:\n{rendered}"
    );
}

/// Every session finding must carry its quote into the report. A finding a reader cannot
/// check against the transcript is an accusation, not a review.
#[test]
fn session_findings_carry_their_quote() {
    let rendered = render_findings_markdown(&result_with(
        Vec::new(),
        vec![session("abandoned_finding", Remedy::Repair)],
    ));
    assert!(
        rendered.contains("passes even when I break run_headless"),
        "the verbatim quote is what makes the finding checkable:\n{rendered}"
    );
}

/// A speculative fix must be introduced as speculative. A reviewer shown a working diff is
/// far likelier to take it than to go back and test whether the finding behind it was true.
#[test]
fn a_remediation_branch_is_labelled_unverified() {
    let mut result = result_with(
        Vec::new(),
        vec![session("abandoned_finding", Remedy::Repair)],
    );
    result.remediation = Some(RemediationRecord {
        branch: "agent/remediation-x".into(),
        outcome: Outcome::Succeeded,
        summary: "rewrote the test".into(),
        addressed: vec!["abandoned_finding".into()],
    });
    let rendered = render_findings_markdown(&result);
    let findings_at = rendered.find("passes even when").expect("finding shown");
    let fix_at = rendered.find("agent/remediation-x").expect("branch shown");
    assert!(
        findings_at < fix_at,
        "the finding must be read before the fix that assumes it:\n{rendered}"
    );
    assert!(
        rendered.contains("unverified"),
        "a fix for an unproven finding must say so:\n{rendered}"
    );
}

/// `CoderTuning::run_config` has silently dropped seven settings before now — the value
/// parses, reaches nobody, and changing it does nothing. This is the check that costs a
/// second and catches it.
#[test]
fn tuning_carries_session_critic_into_the_run_config() {
    let mut tuning = CoderTuning::default();
    tuning.session_critic.enabled = true;
    tuning.session_critic.remediation = true;
    tuning.session_critic.include_tool_names = false;
    let config = tuning.run_config();
    assert_eq!(
        config.session_critic, tuning.session_critic,
        "the setting parsed and then reached nobody"
    );
}

/// `prompt_dir` must survive the conversion, or `[coder] prompt_dir` becomes the ninth
/// setting that parses and reaches nobody.
#[test]
fn tuning_carries_prompt_dir_into_the_run_config() {
    let tuning = CoderTuning {
        prompt_dir: Some("/etc/liberado/prompts".to_string()),
        ..CoderTuning::default()
    };
    assert_eq!(tuning.run_config().prompt_dir, tuning.prompt_dir);
}

/// Unconfigured must mean "the checkout the run is working in", not "the process's cwd".
///
/// The first version of this resolved against cwd and this test caught it: `cargo test` runs
/// with cwd at the crate directory, so the override silently fell back to the baked copy —
/// and would have done the same inside every coding worktree, which is the one place a run
/// most wants the checkout's own prompts.
#[test]
fn an_unconfigured_prompt_dir_resolves_inside_the_workspace() {
    assert!(CoderTuning::default().prompt_dir.is_none());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_string_lossy()
        .to_string();
    let dir = prompts::dir_for(None, &root);
    let from_disk = prompts::load(Some(&dir), prompts::CODER_FILE, "BAKED-FALLBACK");
    assert_ne!(
        from_disk, "BAKED-FALLBACK",
        "a run inside a checkout must read prompts/coder/coder.md from it, not the binary"
    );
}

#[test]
fn a_configured_prompt_dir_is_used_verbatim() {
    assert_eq!(
        prompts::dir_for(Some("/etc/liberado/prompts"), "/some/workspace"),
        Path::new("/etc/liberado/prompts")
    );
}

/// The dangerous toggle must be off unless someone asks for it.
#[test]
fn remediation_is_off_by_default() {
    let config = SessionCriticConfig::default();
    assert!(!config.enabled, "the reviewer itself is opt-in");
    assert!(
        !config.remediation,
        "auto-fixing an unverified finding must never be the default"
    );
    assert!(
        config.include_tool_names,
        "dropping tool names cost two of four labelled traces; it must not be the default"
    );
}
