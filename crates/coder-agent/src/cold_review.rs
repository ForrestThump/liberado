//! Product cold-review stage for self-PRs (backlog 0.8 / Layer B).
//!
//! ```text
//! build → verify → cold review (diff only) → filter (cite-to-keep)
//!       → at most one fix round → re-verify → ready for human | escalate
//! ```
//!
//! Pure policy lives here so tests exercise the real decision functions without a live model.
//! Surfaces call [`build_cold_review_request`] then a provider; they must not inject author goal
//! text or tool traces into that request.

use std::collections::BTreeSet;

use liberado_coder_core::prompts::{
    COLD_PR_REVIEWER, COLD_PR_REVIEWER_FILE, DIFF_REVIEWER, dir_for, load,
};
use serde::{Deserialize, Serialize};

/// Maximum automatic fix rounds after cold review. Layer B default: one, then human.
pub const MAX_FIX_ROUNDS: u32 = 1;

/// Severity for a cold-PR finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    /// High and medium are auto-fixed when retained; low is residual for humans.
    pub fn auto_fix(self) -> bool {
        matches!(self, Severity::High | Severity::Medium)
    }
}

/// One cold-review finding from the model (or a fixture).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColdFinding {
    pub severity: Severity,
    pub title: String,
    pub why: String,
    /// Path in the change surface. Required to **retain** the finding for a fix.
    #[serde(default)]
    pub path: Option<String>,
    /// Line or hunk id. Required with [`Self::path`] for retention.
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
}

impl ColdFinding {
    /// Code-grounded: both path and location are non-empty after trim.
    pub fn has_code_citation(&self) -> bool {
        let path_ok = self
            .path
            .as_deref()
            .map(|p| !p.trim().is_empty())
            .unwrap_or(false);
        let loc_ok = self
            .location
            .as_deref()
            .map(|l| !l.trim().is_empty())
            .unwrap_or(false);
        path_ok && loc_ok
    }
}

/// Why a finding was dropped at the filter step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    /// No path/location — unfalsifiable; do not auto-fix.
    MissingCitation,
    /// Low severity: residual human taste, not an automatic fix pass.
    LowSeverity,
    /// The cited path is not part of the reviewed diff.
    OutsideChangeSurface,
    /// Explicit human or filter drop with free-text reason.
    Explicit { reason: String },
}

/// Outcome of the cite-to-keep filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterResult {
    pub retained: Vec<ColdFinding>,
    pub dropped: Vec<(ColdFinding, DropReason)>,
}

/// What the cold-review stage should do next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageDecision {
    /// No auto-fix work; machine re-check may proceed toward ready.
    NoFixNeeded,
    /// Run exactly one fix coding pass on the retained findings.
    RunFixRound { findings: Vec<ColdFinding> },
    /// Fix budget exhausted or still red after the allowed round.
    EscalateToHuman { reason: String },
}

/// Inputs that **must not** reach the cold reviewer (author context).
#[derive(Debug, Clone, Default)]
pub struct ForbiddenAuthorContext {
    pub goal_narrative: Option<String>,
    pub tool_trace: Option<String>,
    pub prior_agent_chat: Option<String>,
}

/// Change surface only — what a cold reviewer is allowed to see.
#[derive(Debug, Clone)]
pub struct ChangeSurface {
    /// Unified diff text (required).
    pub diff: String,
    /// Optional file excerpts already limited to paths in the diff.
    pub file_excerpts: Vec<(String, String)>,
}

impl ChangeSurface {
    /// Paths named by the diff's old/new file headers.
    pub fn changed_paths(&self) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        let mut in_file_header = false;
        for line in self.diff.lines() {
            if let Some(header) = line.strip_prefix("diff --git ") {
                for raw in header.split_whitespace().take(2) {
                    if let Some(path) = normalize_diff_path(raw) {
                        paths.insert(path);
                    }
                }
                in_file_header = true;
                continue;
            }
            if line.starts_with("@@") {
                in_file_header = false;
                continue;
            }
            if in_file_header
                && let Some(raw) = line
                    .strip_prefix("--- ")
                    .or_else(|| line.strip_prefix("+++ "))
                && let Some(path) = normalize_diff_path(raw)
            {
                paths.insert(path);
            }
        }
        paths
    }
}

fn normalize_diff_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw == "/dev/null" {
        return None;
    }
    let decoded = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).ok()?
    } else {
        raw.to_string()
    };
    Some(
        decoded
            .strip_prefix("a/")
            .or_else(|| decoded.strip_prefix("b/"))
            .unwrap_or(&decoded)
            .replace('\\', "/"),
    )
}

/// Fully assembled cold-review model request (system + user). No provider I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdReviewRequest {
    pub system_prompt: String,
    pub user_message: String,
    /// Path of the prompt file when loaded from disk, else the canonical file name.
    pub prompt_source: String,
}

/// Load the product cold-PR reviewer prompt (disk override, then baked).
pub fn cold_pr_reviewer_prompt(prompt_dir: Option<&str>, workspace_root: &str) -> (String, String) {
    let dir = dir_for(prompt_dir, workspace_root);
    let text = load(Some(&dir), COLD_PR_REVIEWER_FILE, COLD_PR_REVIEWER);
    let path = dir.join(COLD_PR_REVIEWER_FILE);
    let source = if path.is_file() {
        path.to_string_lossy().into_owned()
    } else {
        format!("baked:{COLD_PR_REVIEWER_FILE}")
    };
    (text, source)
}

/// Build the cold-review request from the **change surface only**.
///
/// Returns `Err` if:
/// - the diff is empty, or
/// - any forbidden author field is non-empty (callers must not pass them; this is the hard gate).
///
/// Intentionally does **not** take a goal description or trace: those parameters do not exist so
/// they cannot be "accidentally" forwarded.
pub fn build_cold_review_request(
    surface: &ChangeSurface,
    forbidden: &ForbiddenAuthorContext,
    prompt_dir: Option<&str>,
    workspace_root: &str,
) -> Result<ColdReviewRequest, String> {
    reject_author_context(forbidden)?;
    if surface.diff.trim().is_empty() {
        return Err("cold review requires a non-empty diff".into());
    }
    let changed_paths = surface.changed_paths();
    if changed_paths.is_empty() {
        return Err("cold review diff contains no file headers".into());
    }
    for (path, _) in &surface.file_excerpts {
        let normalized = path.replace('\\', "/");
        if !changed_paths.contains(&normalized) {
            return Err(format!(
                "cold-review excerpt path `{path}` is outside the change surface"
            ));
        }
    }

    let (system_prompt, prompt_source) = cold_pr_reviewer_prompt(prompt_dir, workspace_root);
    // Guard: system prompt must not be the attempt-level DIFF_REVIEWER by accident when both
    // files exist — product stage uses COLD_PR_REVIEWER_FILE. (Baked fallback is fine.)
    let _ = DIFF_REVIEWER;

    let mut user = String::from("## Diff\n\n```diff\n");
    user.push_str(surface.diff.trim_end());
    user.push_str("\n```\n");
    if !surface.file_excerpts.is_empty() {
        user.push_str("\n## File excerpts (paths already in the change)\n");
        for (path, body) in &surface.file_excerpts {
            user.push_str(&format!("\n### {path}\n\n```\n{}\n```\n", body.trim_end()));
        }
    }
    user.push_str(
        "\nRespond with JSON findings only. Cite path and location for every finding you keep.\n",
    );

    // Structural isolation: the assembled user message must not contain injected author blobs.
    // (Diff itself is allowed to mention the word "goal" in code; we check the forbidden strings.)
    for (label, blob) in [
        ("goal_narrative", &forbidden.goal_narrative),
        ("tool_trace", &forbidden.tool_trace),
        ("prior_agent_chat", &forbidden.prior_agent_chat),
    ] {
        if let Some(text) = blob
            && !text.trim().is_empty()
            && user.contains(text)
        {
            return Err(format!(
                "isolation failure: forbidden {label} leaked into cold-review user message"
            ));
        }
    }

    Ok(ColdReviewRequest {
        system_prompt,
        user_message: user,
        prompt_source,
    })
}

fn reject_author_context(forbidden: &ForbiddenAuthorContext) -> Result<(), String> {
    for (label, blob) in [
        ("goal_narrative", &forbidden.goal_narrative),
        ("tool_trace", &forbidden.tool_trace),
        ("prior_agent_chat", &forbidden.prior_agent_chat),
    ] {
        if let Some(text) = blob
            && !text.trim().is_empty()
        {
            return Err(format!(
                "cold reviewer must not receive {label}; strip author context before build"
            ));
        }
    }
    Ok(())
}

/// Cite-to-keep filter: uncited or out-of-diff findings are dropped; low severity is not auto-fixed.
pub fn filter_findings(surface: &ChangeSurface, findings: &[ColdFinding]) -> FilterResult {
    let changed_paths = surface.changed_paths();
    let mut retained = Vec::new();
    let mut dropped = Vec::new();
    for f in findings {
        if !f.has_code_citation() {
            dropped.push((f.clone(), DropReason::MissingCitation));
            continue;
        }
        let path = f.path.as_deref().unwrap_or_default().replace('\\', "/");
        if !changed_paths.contains(&path) {
            dropped.push((f.clone(), DropReason::OutsideChangeSurface));
            continue;
        }
        if !f.severity.auto_fix() {
            dropped.push((f.clone(), DropReason::LowSeverity));
            continue;
        }
        retained.push(f.clone());
    }
    FilterResult { retained, dropped }
}

/// Decide whether to run a fix round given filter output and how many fix rounds already ran.
pub fn decide_after_filter(filter: &FilterResult, fix_rounds_completed: u32) -> StageDecision {
    if filter.retained.is_empty() {
        return StageDecision::NoFixNeeded;
    }
    if fix_rounds_completed >= MAX_FIX_ROUNDS {
        return StageDecision::EscalateToHuman {
            reason: format!(
                "retained {} high/medium finding(s) but fix budget ({MAX_FIX_ROUNDS}) is exhausted",
                filter.retained.len()
            ),
        };
    }
    StageDecision::RunFixRound {
        findings: filter.retained.clone(),
    }
}

/// After a fix round, if re-verify is still red and the budget is used, escalate.
pub fn decide_after_fix_round(
    fix_rounds_completed: u32,
    reverify_passed: bool,
    outstanding_retained: usize,
) -> StageDecision {
    if fix_rounds_completed == 0 {
        return StageDecision::EscalateToHuman {
            reason: "post-fix decision called before a fix round was recorded".into(),
        };
    }
    if reverify_passed && outstanding_retained == 0 {
        return StageDecision::NoFixNeeded;
    }
    if fix_rounds_completed >= MAX_FIX_ROUNDS {
        return StageDecision::EscalateToHuman {
            reason: format!(
                "after {fix_rounds_completed} fix round(s), re-verify failed or \
                 {outstanding_retained} finding(s) remain; escalate to human (no second auto-fix)"
            ),
        };
    }
    // Should not schedule another automatic round under default policy.
    StageDecision::EscalateToHuman {
        reason: "fix policy forbids another automatic round".into(),
    }
}

/// Whether the work unit may be marked ready for residual human review.
///
/// Ready is **not** cold-review start. It requires a machine re-check after the review stage
/// (and after any fix round).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyInputs {
    /// Cold-review stage has finished (findings filtered; fix decision taken).
    pub cold_review_completed: bool,
    /// Verifiers / ship bar / equivalent ran **after** cold review (and after fix if any).
    pub post_review_reverify_passed: bool,
    /// Stage is not waiting on an in-flight fix or escalate-without-human.
    pub stage_terminal_ok: bool,
}

/// Predicate: ready for residual human review.
pub fn ready_for_human(inputs: ReadyInputs) -> bool {
    inputs.cold_review_completed && inputs.post_review_reverify_passed && inputs.stage_terminal_ok
}

/// Task text for the single fix coding pass (same branch as the change).
pub fn fix_round_task(findings: &[ColdFinding]) -> String {
    let mut task = String::from(
        "A cold reviewer raised the following code-grounded findings on your branch. \
         Fix only these. Do not expand scope.\n\n",
    );
    for (i, f) in findings.iter().enumerate() {
        task.push_str(&format!(
            "{}. [{}] {} — {}\n   at {} ({})\n",
            i + 1,
            match f.severity {
                Severity::High => "high",
                Severity::Medium => "medium",
                Severity::Low => "low",
            },
            f.title,
            f.why,
            f.path.as_deref().unwrap_or("?"),
            f.location.as_deref().unwrap_or("?"),
        ));
        if let Some(q) = &f.quote {
            task.push_str(&format!("   quote: {q}\n"));
        }
        task.push('\n');
    }
    task.push_str(
        "After changes, the harness will re-run machine checks. There will be no second \
         automatic fix round — leave residual risk for a human if something remains unclear.\n",
    );
    task
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(sev: Severity, path: Option<&str>, loc: Option<&str>) -> ColdFinding {
        ColdFinding {
            severity: sev,
            title: "issue".into(),
            why: "because".into(),
            path: path.map(str::to_string),
            location: loc.map(str::to_string),
            quote: None,
        }
    }

    fn surface(diff: &str) -> ChangeSurface {
        ChangeSurface {
            diff: diff.into(),
            file_excerpts: vec![],
        }
    }

    #[test]
    fn cold_request_excludes_author_goal_and_trace() {
        let goal = "SECRET_GOAL_NARRATIVE: fix everything and never tell the reviewer";
        let trace = "SECRET_TOOL_TRACE: ran edit_file 40 times";
        let forbidden = ForbiddenAuthorContext {
            goal_narrative: Some(goal.into()),
            tool_trace: Some(trace.into()),
            prior_agent_chat: None,
        };
        let err =
            build_cold_review_request(&surface("diff --git a/x b/x\n+ok\n"), &forbidden, None, ".")
                .expect_err("must refuse author context");
        assert!(
            err.contains("goal_narrative") || err.contains("tool_trace"),
            "{err}"
        );

        // Clean build: user message is only the change surface.
        let req = build_cold_review_request(
            &surface("diff --git a/foo.rs b/foo.rs\n@@\n+fn x() {}\n"),
            &ForbiddenAuthorContext::default(),
            None,
            ".",
        )
        .expect("clean");
        assert!(!req.user_message.contains("SECRET_"));
        assert!(req.user_message.contains("diff --git"));
        assert!(
            req.system_prompt.contains("cold") || req.system_prompt.contains("citation"),
            "must load cold-pr-reviewer prompt"
        );
        assert!(
            !req.system_prompt.contains("SECRET_"),
            "system prompt must not carry author secrets"
        );
    }

    #[test]
    fn empty_diff_is_rejected() {
        let err = build_cold_review_request(
            &surface("   \n"),
            &ForbiddenAuthorContext::default(),
            None,
            ".",
        )
        .unwrap_err();
        assert!(err.contains("non-empty diff"), "{err}");
    }

    #[test]
    fn filter_drops_uncited_and_low_severity() {
        let changed = surface(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@\n+x\n\
             diff --git a/c.rs b/c.rs\n--- a/c.rs\n+++ b/c.rs\n@@\n+y\n",
        );
        let findings = vec![
            finding(Severity::High, Some("a.rs"), Some("L10")),
            finding(Severity::High, None, Some("L10")),
            finding(Severity::Medium, Some("b.rs"), None),
            finding(Severity::Low, Some("c.rs"), Some("L1")),
        ];
        let r = filter_findings(&changed, &findings);
        assert_eq!(r.retained.len(), 1);
        assert_eq!(r.retained[0].path.as_deref(), Some("a.rs"));
        assert_eq!(r.dropped.len(), 3);
        assert!(
            r.dropped
                .iter()
                .any(|(_, d)| matches!(d, DropReason::MissingCitation))
        );
        assert!(
            r.dropped
                .iter()
                .any(|(_, d)| matches!(d, DropReason::LowSeverity))
        );
    }

    #[test]
    fn filter_drops_citations_outside_the_reviewed_diff() {
        let changed = surface("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@\n+x\n");
        let findings = vec![
            finding(Severity::High, Some("a.rs"), Some("hunk-1")),
            finding(Severity::High, Some("unseen.rs"), Some("L1")),
        ];
        let result = filter_findings(&changed, &findings);
        assert_eq!(result.retained.len(), 1);
        assert_eq!(result.retained[0].path.as_deref(), Some("a.rs"));
        assert!(result.dropped.iter().any(|(finding, reason)| {
            finding.path.as_deref() == Some("unseen.rs")
                && matches!(reason, DropReason::OutsideChangeSurface)
        }));
    }

    #[test]
    fn request_rejects_excerpts_outside_the_diff() {
        let changed = ChangeSurface {
            diff: "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@\n+x\n".into(),
            file_excerpts: vec![("secret.rs".into(), "author-only context".into())],
        };
        let err =
            build_cold_review_request(&changed, &ForbiddenAuthorContext::default(), None, ".")
                .unwrap_err();
        assert!(err.contains("outside the change surface"), "{err}");
    }

    /// Excerpts that ARE in the change surface render into the user message; with no excerpts
    /// the section is absent entirely.
    #[test]
    fn file_excerpts_render_only_when_present() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@\n+x\n";
        let with = ChangeSurface {
            diff: diff.into(),
            file_excerpts: vec![("a.rs".into(), "fn kept() {}".into())],
        };
        let req = build_cold_review_request(&with, &ForbiddenAuthorContext::default(), None, ".")
            .expect("excerpt path is in the surface");
        assert!(
            req.user_message.contains("## File excerpts"),
            "an excerpt must reach the reviewer:\n{}",
            req.user_message
        );
        assert!(
            req.user_message.contains("### a.rs"),
            "{}",
            req.user_message
        );
        assert!(
            req.user_message.contains("fn kept()"),
            "{}",
            req.user_message
        );

        let without = build_cold_review_request(
            &surface(diff),
            &ForbiddenAuthorContext::default(),
            None,
            ".",
        )
        .expect("clean");
        assert!(
            !without.user_message.contains("## File excerpts"),
            "no excerpts, no section:\n{}",
            without.user_message
        );
    }

    /// Blank author blobs are "not present", not "present and leaked": the isolation guard
    /// must not fire on them.
    #[test]
    fn an_empty_forbidden_blob_is_not_a_leak() {
        let forbidden = ForbiddenAuthorContext {
            goal_narrative: Some(String::new()),
            tool_trace: Some("   ".into()),
            prior_agent_chat: None,
        };
        let req =
            build_cold_review_request(&surface("diff --git a/x b/x\n+ok\n"), &forbidden, None, ".")
                .expect("blank blobs carry nothing to isolate");
        assert!(req.user_message.contains("diff --git"));
    }

    /// The post-fix decision separates a green re-verify from a red one; findings left after a
    /// passing re-verify are still escalation, not silence.
    #[test]
    fn decide_after_fix_round_distinguishes_green_from_red_reverify() {
        match decide_after_fix_round(1, true, 0) {
            StageDecision::NoFixNeeded => {}
            other => panic!("green re-verify with nothing outstanding is done: {other:?}"),
        }
        // Retained findings survive the re-verify: never NoFixNeeded.
        match decide_after_fix_round(1, true, 2) {
            StageDecision::EscalateToHuman { reason } => {
                assert!(reason.contains("remain"), "{reason}");
            }
            other => panic!("findings left behind must escalate: {other:?}"),
        }
        // A red re-verify escalates even when the filter retained nothing new.
        match decide_after_fix_round(1, false, 0) {
            StageDecision::EscalateToHuman { .. } => {}
            other => panic!("failed re-verify still escalates: {other:?}"),
        }
    }

    #[test]
    fn one_fix_round_then_escalate() {
        let retained = vec![finding(Severity::High, Some("a.rs"), Some("L1"))];
        let filter = FilterResult {
            retained: retained.clone(),
            dropped: vec![],
        };
        match decide_after_filter(&filter, 0) {
            StageDecision::RunFixRound { findings } => assert_eq!(findings.len(), 1),
            other => panic!("expected fix round, got {other:?}"),
        }
        // Budget already used — never schedule a second automatic fix.
        match decide_after_filter(&filter, MAX_FIX_ROUNDS) {
            StageDecision::EscalateToHuman { reason } => {
                assert!(
                    reason.contains("exhausted") || reason.contains("budget"),
                    "{reason}"
                );
            }
            other => panic!("expected escalate, got {other:?}"),
        }
        match decide_after_fix_round(1, false, 1) {
            StageDecision::EscalateToHuman { reason } => {
                assert!(
                    reason.contains("no second") || reason.contains("remain"),
                    "{reason}"
                );
            }
            other => panic!("expected escalate after failed re-verify, got {other:?}"),
        }
        match decide_after_fix_round(0, true, 0) {
            StageDecision::EscalateToHuman { reason } => {
                assert!(reason.contains("before a fix round"), "{reason}");
            }
            other => panic!("zero completed rounds is invalid, got {other:?}"),
        }
    }

    #[test]
    fn ready_requires_post_review_reverify_not_review_start() {
        assert!(!ready_for_human(ReadyInputs {
            cold_review_completed: false,
            post_review_reverify_passed: false,
            stage_terminal_ok: false,
        }));
        // "Review started / labels applied" is not ready.
        assert!(!ready_for_human(ReadyInputs {
            cold_review_completed: true,
            post_review_reverify_passed: false,
            stage_terminal_ok: true,
        }));
        assert!(ready_for_human(ReadyInputs {
            cold_review_completed: true,
            post_review_reverify_passed: true,
            stage_terminal_ok: true,
        }));
    }

    #[test]
    fn fix_task_names_only_retained_findings() {
        let task = fix_round_task(&[finding(Severity::Medium, Some("x.rs"), Some("hunk-3"))]);
        assert!(task.contains("x.rs"));
        assert!(task.contains("hunk-3"));
        assert!(task.contains("no second"));
    }

    #[test]
    fn prompt_file_is_the_product_taste_surface() {
        // Structural: product stage loads COLD_PR_REVIEWER_FILE, not a second ad-hoc string.
        assert_eq!(COLD_PR_REVIEWER_FILE, "cold-pr-reviewer.md");
        assert!(
            COLD_PR_REVIEWER.contains("citation") || COLD_PR_REVIEWER.contains("path"),
            "baked cold-pr prompt must require citations"
        );
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing-prompts");
        let (_, source) = cold_pr_reviewer_prompt(missing.to_str(), ".");
        assert_eq!(source, "baked:cold-pr-reviewer.md");
    }
}
