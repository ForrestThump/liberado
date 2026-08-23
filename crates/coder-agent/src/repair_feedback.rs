//! Failure-signature routing for repair attempts.
//!
//! Turns verifier pipeline failures (and other retryable errors) into structured prior_feedback
//! so the repair role sees a stable signature + class, not only free-form text.

use liberado_coder_core::{CoderError, FindingKind, PipelineResult};

/// Coarse class of failure for repair routing (stable across wording tweaks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    NoChanges,
    MissingPath,
    ContentMismatch,
    CommandFailed,
    CommandTimeout,
    EmptyDiff,
    CriticRevision,
    ValidationOther,
    /// The machine failed, not the change. Disk exhaustion, a killed linker, a read-only tree.
    ///
    /// Kept apart from `CommandFailed` because the remedy is not the model's to apply. A real
    /// run ended this way: the disk filled mid-session, `cargo` exited 101 with "no space on
    /// device", and the harness answered "reproduce the failing command locally, fix the root
    /// cause, re-run until green". The model spent its remaining turns on a fault it could not
    /// reach, and the run was filed as though the change were wrong.
    Infrastructure,
    Other,
}

impl FailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoChanges => "no_changes",
            Self::MissingPath => "missing_path",
            Self::ContentMismatch => "content_mismatch",
            Self::CommandFailed => "command_failed",
            Self::CommandTimeout => "command_timeout",
            Self::EmptyDiff => "empty_diff",
            Self::CriticRevision => "critic_revision",
            Self::ValidationOther => "validation_other",
            Self::Infrastructure => "infrastructure",
            Self::Other => "other",
        }
    }

    pub fn repair_hint(self) -> &'static str {
        match self {
            Self::NoChanges => {
                "Make a real workspace mutation (write_file/edit_file/apply_patch), or commit \
                 changes with git_commit if you already edited — uncommitted *or* commits since \
                 attempt start both count as progress."
            }
            Self::MissingPath => {
                "Create the missing paths listed in findings before claiming success."
            }
            Self::ContentMismatch => {
                "Edit the named files so they contain the required strings/symbols."
            }
            Self::CommandFailed => {
                "The ship bar ran the command named in FINDINGS (usually `cargo check` then \
                 `cargo test --workspace`). Reproduce *that* command — a green \
                 `cargo test -p <one-crate>` is not the bar. Do not pass shell tokens \
                 (`2>&1`, `|`, `&&`) as cargo arguments. Fix the root cause, re-run until green."
            }
            Self::CommandTimeout => {
                "Speed up or simplify the failing command; avoid infinite loops in scripts."
            }
            Self::EmptyDiff => {
                "Ensure files are written under the workspace and show in git status."
            }
            Self::CriticRevision => {
                "Address each critic issue on the actual diff; do not re-argue in prose only."
            }
            Self::Infrastructure => {
                "The build environment failed, not your change. Do not try to repair this — stop \
                 and report it so an operator can act."
            }
            Self::ValidationOther | Self::Other => {
                "Read the findings carefully; change approach if the same signature already failed."
            }
        }
    }
}

/// Signatures of a machine that cannot build, whatever the change is.
///
/// Matched against a command's captured output rather than its exit code, because the exit code
/// is the same 101 whether the crate does not compile or the disk is full. Deliberately short:
/// every entry is a phrase a toolchain prints when the *host* has failed, and a phrase that can
/// also come out of a legitimately broken build would turn a real failure into a shrug.
const INFRASTRUCTURE_SIGNS: &[&str] = &[
    "no space left on device",
    "no space on device",
    "not enough space",
    "insufficient disk space",
    "cannot allocate memory",
    "read-only file system",
    "too many open files",
];

/// Whether a captured command output says the machine failed rather than the change.
pub fn looks_like_infrastructure_failure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    INFRASTRUCTURE_SIGNS.iter().any(|s| lower.contains(s))
}

/// Build agent-facing repair feedback from a failed pipeline (includes signature + class).
pub fn format_pipeline_repair(pipeline: &PipelineResult) -> String {
    let class = classify_pipeline(pipeline);
    let signature = pipeline
        .combined_signature
        .clone()
        .unwrap_or_else(|| "unknown".into());
    let mut lines = vec![
        format!("FAILURE_CLASS: {}", class.as_str()),
        format!("FAILURE_SIGNATURE: {signature}"),
        format!("REPAIR_HINT: {}", class.repair_hint()),
        "FINDINGS:".into(),
    ];
    if pipeline.combined_findings.is_empty() {
        for r in &pipeline.results {
            if !r.verdict.is_pass() {
                lines.push(format!("- [{}] {}: {}", r.kind, r.id, r.verdict.summary));
            }
        }
    } else {
        for f in &pipeline.combined_findings {
            lines.push(format!("- [{:?}] {}: {}", f.kind, f.check_id, f.message));
        }
    }
    for r in &pipeline.results {
        if r.verdict.is_pass() {
            continue;
        }
        let Some(excerpt) = r.verdict.log_excerpt.as_deref() else {
            continue;
        };
        let clipped = clip_log_excerpt(excerpt, 40);
        if clipped.is_empty() {
            continue;
        }
        lines.push(format!("OUTPUT ({}):", r.id));
        lines.push(clipped);
    }
    lines.push("Fix these before claiming success. Prefer a different approach if this signature already failed.".into());
    lines.join("\n")
}

/// A bounded verifier-log excerpt that keeps failure evidence ahead of routine tail output.
///
/// Workspace `cargo test --no-fail-fast` can print a failing crate and then hundreds of lines
/// from later, passing crates. A plain tail made compares 4 and 9 tell the repair role only that
/// `wire` passed 61 tests. Cargo's final `error: test failed, to rerun pass '-p <crate>'` line is
/// preferred because it names the package; test failures, panics, and compiler errors follow.
/// Unknown output retains the old tail fallback.
pub(crate) fn clip_log_excerpt(excerpt: &str, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let lines: Vec<&str> = excerpt.lines().collect();
    if lines.len() <= max_lines {
        return excerpt.trim().to_string();
    }

    let package_markers = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| is_package_failure_marker(line).then_some(idx));
    let other_markers = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| is_failure_marker(line).then_some(idx));

    let mut seen = std::collections::BTreeSet::new();
    let mut selected = Vec::new();
    for anchor in package_markers.chain(other_markers) {
        let start = anchor.saturating_sub(2);
        let end = (anchor + 3).min(lines.len());
        for idx in start..end {
            if selected.len() == max_lines {
                break;
            }
            if seen.insert(idx) {
                selected.push(idx);
            }
        }
        if selected.len() == max_lines {
            break;
        }
    }

    if selected.is_empty() {
        return lines[lines.len() - max_lines..].join("\n");
    }
    selected
        .into_iter()
        .map(|idx| lines[idx])
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_package_failure_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error: test failed") || lower.contains("could not compile")
}

fn is_failure_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    is_package_failure_marker(line)
        || line.contains(" FAILED")
        || lower.contains("error[")
        || lower.contains("error:")
        || lower.contains("panicked at")
        || lower.contains("test result: failed")
}

/// Drop prior-attempt verifier blocks that a later attempt already cleared.
///
/// Compare 3 attempt 2 still carried attempt 0's `cargo-check` 101 after check
/// was green. The model went back to a compile problem that was gone.
pub fn prune_resolved_verifier_feedback(prior: &mut Vec<String>, latest: &str) {
    let latest_fails_check = latest.contains("cargo-check:");
    if !latest_fails_check {
        prior.retain(|old| !old.contains("cargo-check:"));
    }
}

pub fn classify_pipeline(pipeline: &PipelineResult) -> FailureClass {
    // First, because it outranks every other reading of the same failure. A full disk and a
    // genuine compile error both leave `cargo` at exit 101 with a `CommandFailed` finding; only
    // the captured output tells them apart, and getting it wrong sends the model to repair a
    // fault it cannot reach.
    for r in &pipeline.results {
        if r.verdict.is_pass() {
            continue;
        }
        if let Some(log) = &r.verdict.log_excerpt
            && looks_like_infrastructure_failure(log)
        {
            return FailureClass::Infrastructure;
        }
    }
    for f in &pipeline.combined_findings {
        match f.kind {
            FindingKind::MissingPath => return FailureClass::MissingPath,
            FindingKind::ContentMismatch => return FailureClass::ContentMismatch,
            FindingKind::CommandTimeout => return FailureClass::CommandTimeout,
            FindingKind::CommandFailed => return FailureClass::CommandFailed,
            FindingKind::EmptyDiff => return FailureClass::EmptyDiff,
            FindingKind::PolicyDenied | FindingKind::UnexpectedChange | FindingKind::Custom => {}
        }
    }
    for r in &pipeline.results {
        if r.verdict.is_pass() {
            continue;
        }
        match r.kind.as_str() {
            "paths_exist" | "paths_absent" => return FailureClass::MissingPath,
            "content_contains" => return FailureClass::ContentMismatch,
            "command" => return FailureClass::CommandFailed,
            "git_nonempty_diff" => return FailureClass::EmptyDiff,
            _ => {}
        }
    }
    FailureClass::ValidationOther
}

/// Classify a retryable error for prior_feedback routing.
pub fn classify_error(err: &CoderError) -> FailureClass {
    match err {
        CoderError::NoChanges => FailureClass::NoChanges,
        CoderError::Validation(msg) => classify_message(msg),
        CoderError::Backend(msg) if msg.contains("critic") => FailureClass::CriticRevision,
        _ => FailureClass::Other,
    }
}

/// Rule table for messages that carry an explicit `FAILURE_CLASS:` marker. These take precedence
/// because such messages also quote the findings that produced them — a disk-full run carries both
/// `infrastructure` and the `CommandFailed` text that caused it.
const CLASSED_RULES: &[(&[&str], FailureClass)] = &[
    (&["infrastructure"], FailureClass::Infrastructure),
    (&["no_changes"], FailureClass::NoChanges),
    (&["missing_path"], FailureClass::MissingPath),
    (&["content_mismatch"], FailureClass::ContentMismatch),
    (&["command_timeout"], FailureClass::CommandTimeout),
    (&["command_failed"], FailureClass::CommandFailed),
    (&["empty_diff"], FailureClass::EmptyDiff),
    (&["critic_revision"], FailureClass::CriticRevision),
];

/// Rule table for unmarked messages, matched in declaration order — the first hit wins.
const GENERIC_RULES: &[(&[&str], FailureClass)] = &[
    (
        &["no real workspace changes", "no_changes"],
        FailureClass::NoChanges,
    ),
    (&["missing path", "missing_path"], FailureClass::MissingPath),
    (&["must contain", "content"], FailureClass::ContentMismatch),
    (&["timed out"], FailureClass::CommandTimeout),
    (&["exited", "command"], FailureClass::CommandFailed),
    (&["critic"], FailureClass::CriticRevision),
    (
        &["completeness", "validation"],
        FailureClass::ValidationOther,
    ),
];

/// The first rule whose keyword (lower-cased) appears in `lower` is the class.
fn match_rules(lower: &str, rules: &[(&[&str], FailureClass)]) -> Option<FailureClass> {
    rules
        .iter()
        .find(|(needles, _)| needles.iter().any(|n| lower.contains(n)))
        .map(|(_, class)| *class)
}

pub fn classify_message(msg: &str) -> FailureClass {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("failure_class:")
        && let Some(class) = match_rules(&lower, CLASSED_RULES)
    {
        return class;
    }
    match_rules(&lower, GENERIC_RULES).unwrap_or(FailureClass::Other)
}

/// Format prior_feedback entry for a retryable error (signature-aware).
pub fn format_error_feedback(err: &CoderError) -> String {
    match err {
        CoderError::Validation(msg) if msg.contains("FAILURE_CLASS:") => msg.clone(),
        CoderError::Validation(msg) => {
            let class = classify_message(msg);
            format!(
                "FAILURE_CLASS: {}\nFAILURE_SIGNATURE: validation:{}\nREPAIR_HINT: {}\nDETAIL:\n{}",
                class.as_str(),
                short_sig(msg),
                class.repair_hint(),
                msg
            )
        }
        CoderError::NoChanges => {
            let class = FailureClass::NoChanges;
            format!(
                "FAILURE_CLASS: {}\nFAILURE_SIGNATURE: no_changes\nREPAIR_HINT: {}\nDETAIL: no real workspace changes were produced",
                class.as_str(),
                class.repair_hint()
            )
        }
        other => {
            let class = classify_error(other);
            format!(
                "FAILURE_CLASS: {}\nFAILURE_SIGNATURE: {}\nREPAIR_HINT: {}\nDETAIL: {}",
                class.as_str(),
                short_sig(&other.to_string()),
                class.repair_hint(),
                other
            )
        }
    }
}

/// Extra goal section for the repair role from prior_feedback lines.
pub fn repair_focus_block(prior_feedback: &[String]) -> Option<String> {
    if prior_feedback.is_empty() {
        return None;
    }
    let last = prior_feedback.last()?;
    let class = classify_message(last);
    let mut out = String::from("## Repair focus (failure-signature routing)\n");
    out.push_str("You are on a REPAIR attempt. Do not restart from zero.\n");
    out.push_str("Primary class: ");
    out.push_str(class.as_str());
    out.push('\n');
    out.push_str("Hint: ");
    out.push_str(class.repair_hint());
    out.push_str("\n\nLatest failure detail:\n");
    out.push_str(last);
    out.push('\n');
    if prior_feedback.len() > 1 {
        out.push_str("\nEarlier attempts (avoid repeating failed approaches):\n");
        for (i, fb) in prior_feedback
            .iter()
            .enumerate()
            .take(prior_feedback.len() - 1)
        {
            out.push_str(&format!("- attempt {}: {}\n", i + 1, first_line(fb)));
        }
    }
    Some(out)
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

fn short_sig(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.chars().take(400).collect::<String>().as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::{Finding, FindingKind, NamedVerdict, Verdict, VerdictStatus};

    #[test]
    fn pipeline_missing_path_class() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![NamedVerdict {
                id: "paths".into(),
                kind: "paths_exist".into(),
                verdict: Verdict::fail(
                    "missing",
                    vec![Finding {
                        check_id: "paths".into(),
                        kind: FindingKind::MissingPath,
                        message: "missing path: src/main.rs".into(),
                        detail: None,
                    }],
                    None,
                ),
            }],
            combined_findings: vec![Finding {
                check_id: "paths".into(),
                kind: FindingKind::MissingPath,
                message: "missing path: src/main.rs".into(),
                detail: None,
            }],
            combined_signature: Some("abc".into()),
        };
        let fb = format_pipeline_repair(&pipeline);
        assert!(fb.contains("FAILURE_CLASS: missing_path"));
        assert!(fb.contains("FAILURE_SIGNATURE: abc"));
        assert!(fb.contains("src/main.rs"));
    }

    #[test]
    fn no_changes_feedback() {
        let fb = format_error_feedback(&CoderError::NoChanges);
        assert!(fb.contains("no_changes"));
        assert!(fb.contains("REPAIR_HINT"));
    }

    #[test]
    fn repair_focus_from_prior() {
        let prior = vec![format_error_feedback(&CoderError::NoChanges)];
        let block = repair_focus_block(&prior).unwrap();
        assert!(block.contains("REPAIR attempt"));
        assert!(block.contains("no_changes"));
    }

    #[test]
    fn repair_focus_block_with_multiple_attempts() {
        let prior = vec![
            format_error_feedback(&CoderError::NoChanges),
            format_pipeline_repair(&PipelineResult {
                overall: VerdictStatus::Fail,
                results: vec![],
                combined_findings: vec![Finding {
                    check_id: "paths".into(),
                    kind: FindingKind::MissingPath,
                    message: "missing".into(),
                    detail: None,
                }],
                combined_signature: Some("def".into()),
            }),
        ];
        let block = repair_focus_block(&prior).unwrap();
        assert!(block.contains("attempt 1"), "got: {block}");
    }

    #[test]
    fn as_str_and_repair_hint_all_variants() {
        for class in [
            FailureClass::NoChanges,
            FailureClass::MissingPath,
            FailureClass::ContentMismatch,
            FailureClass::CommandFailed,
            FailureClass::CommandTimeout,
            FailureClass::EmptyDiff,
            FailureClass::CriticRevision,
            FailureClass::ValidationOther,
            FailureClass::Other,
        ] {
            assert!(!class.as_str().is_empty(), "no as_str for {class:?}");
            assert!(
                !class.repair_hint().is_empty(),
                "no repair_hint for {class:?}"
            );
        }
    }

    #[test]
    fn classify_message_matches_marked_and_generic_keywords() {
        // A marked class beats the generic text the same message also quotes.
        assert_eq!(
            classify_message("FAILURE_CLASS: infrastructure\nexited: 1"),
            FailureClass::Infrastructure
        );
        assert_eq!(
            classify_message("command timed out"),
            FailureClass::CommandTimeout
        );
        assert_eq!(
            classify_message("missing path: src/main.rs"),
            FailureClass::MissingPath
        );
        assert_eq!(
            classify_message("the critic wants revision"),
            FailureClass::CriticRevision
        );
        assert_eq!(
            classify_message("completeness check failed"),
            FailureClass::ValidationOther
        );
        assert_eq!(classify_message("totally unrelated"), FailureClass::Other);
    }

    #[test]
    fn classify_pipeline_content_mismatch() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![],
            combined_findings: vec![Finding {
                check_id: "content".into(),
                kind: FindingKind::ContentMismatch,
                message: "missing 'TODO' in src/lib.rs".into(),
                detail: None,
            }],
            combined_signature: Some("sig".into()),
        };
        assert_eq!(classify_pipeline(&pipeline), FailureClass::ContentMismatch);
    }

    #[test]
    fn classify_pipeline_command_failed() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![],
            combined_findings: vec![Finding {
                check_id: "cmd".into(),
                kind: FindingKind::CommandFailed,
                message: "cargo test failed".into(),
                detail: None,
            }],
            combined_signature: Some("sig".into()),
        };
        assert_eq!(classify_pipeline(&pipeline), FailureClass::CommandFailed);
    }

    #[test]
    fn classify_pipeline_falls_back_to_results_when_findings_empty() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![NamedVerdict {
                id: "git".into(),
                kind: "git_nonempty_diff".into(),
                verdict: Verdict::fail("nothing committed", vec![], None),
            }],
            combined_findings: vec![],
            combined_signature: Some("sig".into()),
        };
        assert_eq!(classify_pipeline(&pipeline), FailureClass::EmptyDiff);
    }

    #[test]
    fn classify_error_backend_with_critic_is_revision() {
        let err = CoderError::Backend("critic flagged minor issues: trailing whitespace".into());
        assert_eq!(classify_error(&err), FailureClass::CriticRevision);
    }

    /// A failed `cargo` run whose captured output carries `log`.
    ///
    /// The finding says only "cargo exited 101" — exactly as the real verifier writes it — so
    /// these tests exercise the same evidence the classifier actually has.
    fn cargo_failure_with_log(log: &str) -> PipelineResult {
        let finding = Finding {
            check_id: "cargo-check".into(),
            kind: FindingKind::CommandFailed,
            message: "cargo exited 101".into(),
            detail: None,
        };
        PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![NamedVerdict {
                id: "cargo-check".into(),
                kind: "command".into(),
                verdict: Verdict::fail(
                    "cargo exited 101",
                    vec![finding.clone()],
                    Some(log.to_string()),
                ),
            }],
            combined_findings: vec![finding],
            combined_signature: Some("sig".into()),
        }
    }

    /// The run that motivated this. The disk filled mid-session; `cargo` exited 101; the harness
    /// told the model to "fix the root cause", which was a full disk.
    #[test]
    fn a_full_disk_is_not_the_models_fault() {
        let pipeline = cargo_failure_with_log(
            "stdout:\nstderr:\nrustc-LLVM ERROR: IO failure on output stream: No space left on device",
        );
        assert_eq!(
            classify_pipeline(&pipeline),
            FailureClass::Infrastructure,
            "a full disk must outrank the CommandFailed finding"
        );
        let fb = format_pipeline_repair(&pipeline);
        assert!(fb.contains("FAILURE_CLASS: infrastructure"), "{fb}");
        assert!(
            !fb.contains("fix the root cause"),
            "the model must not be sent to repair the machine: {fb}"
        );
    }

    /// The other side of the same coin. A genuine compile error exits 101 too, and must still be
    /// routed to a repair — otherwise this change trades one wrong answer for another.
    #[test]
    fn repair_feedback_includes_the_cargo_excerpt() {
        let pipeline = cargo_failure_with_log(
            "stdout:\nstderr:\nerror[E0425]: cannot find value `foo` in this scope\n\
             test vault_source::tests::hold_flag_parks ... FAILED",
        );
        let fb = format_pipeline_repair(&pipeline);
        assert!(
            fb.contains("error[E0425]"),
            "repair role must see rustc lines, not only 101: {fb}"
        );
        assert!(
            fb.contains("hold_flag_parks"),
            "repair role must see the failing test name: {fb}"
        );
        assert!(fb.contains("OUTPUT (cargo-check):"), "{fb}");
    }

    #[test]
    fn failure_excerpt_beats_a_later_passing_crate() {
        let mut log = vec![
            "running 1 test".to_string(),
            "test checkpoint::tests::resumes_cleanly ... FAILED".to_string(),
            "test result: FAILED. 0 passed; 1 failed; 0 ignored".to_string(),
            "error: test failed, to rerun pass `-p liberado-coder-sandbox --lib`".to_string(),
        ];
        log.extend((0..61).map(|n| format!("test wire::tests::case_{n} ... ok")));

        let clipped = clip_log_excerpt(&log.join("\n"), 12);
        assert!(clipped.contains("resumes_cleanly ... FAILED"), "{clipped}");
        assert!(clipped.contains("liberado-coder-sandbox"), "{clipped}");
        assert!(clipped.lines().count() <= 12, "{clipped}");
    }

    #[test]
    fn unknown_long_output_keeps_the_tail() {
        let log = (0..50)
            .map(|n| format!("ordinary line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let clipped = clip_log_excerpt(&log, 5);
        assert!(!clipped.contains("ordinary line 44"), "{clipped}");
        assert!(clipped.starts_with("ordinary line 45"), "{clipped}");
        assert!(clipped.ends_with("ordinary line 49"), "{clipped}");
    }

    #[test]
    fn prune_drops_stale_cargo_check_when_only_tests_fail() {
        let mut prior = vec![
            "FAILURE_CLASS: command_failed\nFINDINGS:\n- [CommandFailed] cargo-check: cargo exited 101"
                .into(),
        ];
        let latest = "FAILURE_CLASS: command_failed\nFINDINGS:\n- [CommandFailed] cargo-test: cargo exited 101";
        prune_resolved_verifier_feedback(&mut prior, latest);
        assert!(
            prior.is_empty(),
            "stale cargo-check 101 must not ride into the next attempt: {prior:?}"
        );
    }

    #[test]
    fn prune_keeps_cargo_check_when_it_still_fails() {
        let old = "FAILURE_CLASS: command_failed\nFINDINGS:\n- [CommandFailed] cargo-check: cargo exited 101"
            .to_string();
        let mut prior = vec![old.clone()];
        prune_resolved_verifier_feedback(&mut prior, &old);
        assert_eq!(prior.len(), 1);
    }

    #[test]
    fn a_real_compile_error_is_still_the_models_to_fix() {
        let pipeline = cargo_failure_with_log(
            "stdout:\nstderr:\nerror[E0425]: cannot find value `foo` in this scope",
        );
        assert_eq!(classify_pipeline(&pipeline), FailureClass::CommandFailed);
    }

    #[test]
    fn other_host_failures_are_recognised() {
        for log in [
            "fatal: cannot allocate memory",
            "error: Read-only file system (os error 30)",
            "OSError: Too many open files",
        ] {
            assert_eq!(
                classify_pipeline(&cargo_failure_with_log(log)),
                FailureClass::Infrastructure,
                "unhandled host failure: {log}"
            );
        }
    }

    /// Round-trip: what `format_pipeline_repair` writes, `classify_message` must read back.
    ///
    /// The message quotes its own findings, so a disk-full failure carries the word
    /// `CommandFailed` inside the text that classifies it as infrastructure. Whichever check runs
    /// first wins, and the wrong winner sends the repair role after the machine.
    #[test]
    fn the_written_class_survives_being_read_back() {
        let pipeline = cargo_failure_with_log("stderr:\nNo space left on device");
        let written = format_pipeline_repair(&pipeline);
        assert!(written.contains("CommandFailed"), "premise: {written}");
        assert_eq!(
            classify_message(&written),
            FailureClass::Infrastructure,
            "the quoted finding must not outrank the class tag: {written}"
        );
    }

    #[test]
    fn infrastructure_detection_is_case_insensitive() {
        assert!(looks_like_infrastructure_failure("No Space Left On Device"));
        assert!(!looks_like_infrastructure_failure(
            "error[E0308]: mismatched types"
        ));
    }

    #[test]
    fn classify_message_parses_failure_class_tags() {
        assert_eq!(
            classify_message("FAILURE_CLASS: command_timeout\nsome details"),
            FailureClass::CommandTimeout
        );
        assert_eq!(
            classify_message("FAILURE_CLASS: content_mismatch"),
            FailureClass::ContentMismatch
        );
        assert_eq!(
            classify_message("FAILURE_CLASS: empty_diff"),
            FailureClass::EmptyDiff
        );
        assert_eq!(
            classify_message("FAILURE_CLASS: critic_revision"),
            FailureClass::CriticRevision
        );
        assert_eq!(
            classify_message("FAILURE_CLASS: validation_other"),
            FailureClass::ValidationOther
        );
        assert_eq!(classify_message("no tags here"), FailureClass::Other);
    }

    /// Hints are per-class guidance, not one string for everything: the no-changes hint names
    /// workspace mutation, the infrastructure hint forbids self-repair.
    #[test]
    fn repair_hints_differ_per_class() {
        assert!(
            FailureClass::NoChanges
                .repair_hint()
                .contains("workspace mutation"),
            "{}",
            FailureClass::NoChanges.repair_hint()
        );
        assert!(
            FailureClass::Infrastructure
                .repair_hint()
                .contains("Do not try to repair"),
            "{}",
            FailureClass::Infrastructure.repair_hint()
        );
        assert_ne!(
            FailureClass::CommandTimeout.repair_hint(),
            FailureClass::EmptyDiff.repair_hint()
        );
    }

    /// With combined findings absent, the fallback lists only *failing* results — a green
    /// verifier is not a finding.
    #[test]
    fn passing_results_are_not_listed_as_findings_in_the_fallback() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![
                NamedVerdict {
                    id: "ok-check".into(),
                    kind: "command".into(),
                    verdict: Verdict::pass("cargo exited 0"),
                },
                NamedVerdict {
                    id: "bad-check".into(),
                    kind: "command".into(),
                    verdict: Verdict::fail("cargo exited 101", vec![], None),
                },
            ],
            combined_findings: vec![],
            combined_signature: Some("sig".into()),
        };
        let fb = format_pipeline_repair(&pipeline);
        assert!(fb.contains("bad-check"), "{fb}");
        assert!(
            !fb.contains("exited 0"),
            "a passing verifier must not read as a finding: {fb}"
        );
    }

    /// A package-failure marker on the very first line must survive clipping with context;
    /// the anchor window is [anchor-2, anchor+3), never an empty range.
    #[test]
    fn a_marker_on_the_first_line_is_kept_with_context() {
        let mut log = vec!["error: test failed, to rerun pass `-p mycrate --lib`".to_string()];
        log.extend((0..20).map(|n| format!("ordinary line {n}")));
        let clipped = clip_log_excerpt(&log.join("\n"), 5);
        assert!(clipped.contains("-p mycrate"), "{clipped}");
    }

    /// `could not compile` is a package failure without being a generic failure marker; it
    /// must still win its excerpt against a long tail of passing output.
    #[test]
    fn could_not_compile_is_a_package_failure_marker_on_its_own() {
        let mut log = vec!["could not compile `mycrate` (bin \"mycrate\")".to_string()];
        log.extend((0..20).map(|n| format!("ordinary line {n}")));
        let clipped = clip_log_excerpt(&log.join("\n"), 5);
        assert!(clipped.contains("could not compile"), "{clipped}");
    }

    /// Each generic failure-marker kind selects its own line when it is the only marker in
    /// the log; losing any one kind silently degrades repair feedback for that failure.
    #[test]
    fn each_generic_failure_marker_selects_its_line() {
        for (marker_line, needle) in [
            ("test wire::alpha ... FAILED", "wire::alpha"),
            ("error[E0425]: cannot find value `x`", "E0425"),
            ("error: linking with cc failed", "linking with cc"),
            ("panicked at src/lib.rs:7:5:", "panicked at"),
            ("test result: failed. 2 passed; 1 failed", "result: failed"),
        ] {
            let mut log = vec![marker_line.to_string()];
            log.extend((0..15).map(|n| format!("filler {n}")));
            let clipped = clip_log_excerpt(&log.join("\n"), 4);
            assert!(
                clipped.contains(needle),
                "marker lost from the excerpt: {marker_line}\n{clipped}"
            );
        }
    }

    /// Result-kind fallback mapping: with no combined findings, each verifier kind routes to
    /// its class.
    #[test]
    fn classify_pipeline_maps_result_kinds_without_findings() {
        fn fail_of_kind(kind: &str) -> PipelineResult {
            PipelineResult {
                overall: VerdictStatus::Fail,
                results: vec![NamedVerdict {
                    id: "v".into(),
                    kind: kind.into(),
                    verdict: Verdict::fail("nope", vec![], None),
                }],
                combined_findings: vec![],
                combined_signature: Some("sig".into()),
            }
        }
        assert_eq!(
            classify_pipeline(&fail_of_kind("paths_exist")),
            FailureClass::MissingPath
        );
        assert_eq!(
            classify_pipeline(&fail_of_kind("paths_absent")),
            FailureClass::MissingPath
        );
        assert_eq!(
            classify_pipeline(&fail_of_kind("content_contains")),
            FailureClass::ContentMismatch
        );
        assert_eq!(
            classify_pipeline(&fail_of_kind("command")),
            FailureClass::CommandFailed
        );
    }

    #[test]
    fn classify_error_routes_no_changes_validation_and_noncritic_backend() {
        assert_eq!(
            classify_error(&CoderError::NoChanges),
            FailureClass::NoChanges
        );
        assert_eq!(
            classify_error(&CoderError::Validation("missing path: src/main.rs".into())),
            FailureClass::MissingPath,
            "a validation message is classified by its text"
        );
        assert_eq!(
            classify_error(&CoderError::Backend("cargo exited 101".into())),
            FailureClass::Other,
            "a backend error that never mentions the critic is not a critic revision"
        );
    }

    /// An already-marked message round-trips byte-for-byte — re-formatting it would grow the
    /// feedback block on every attempt.
    #[test]
    fn marked_validation_messages_pass_through_untouched() {
        let marked =
            format_pipeline_repair(&cargo_failure_with_log("stderr:\nNo space left on device"));
        let out = format_error_feedback(&CoderError::Validation(marked.clone()));
        assert_eq!(out, marked, "marked messages must pass through unchanged");
    }

    #[test]
    fn unmarked_validation_messages_get_enriched() {
        let out =
            format_error_feedback(&CoderError::Validation("missing path: src/main.rs".into()));
        assert!(out.contains("FAILURE_CLASS: missing_path"), "{out}");
        assert!(out.contains("FAILURE_SIGNATURE:"), "{out}");
        assert!(out.contains("REPAIR_HINT:"), "{out}");
    }

    /// Earlier attempts list every entry except the latest, and only their first lines.
    #[test]
    fn repair_focus_lists_earlier_attempts_but_not_the_latest_twice() {
        let prior = vec![
            format_error_feedback(&CoderError::Validation("command timed out".into())),
            format_error_feedback(&CoderError::NoChanges),
        ];
        let block = repair_focus_block(&prior).unwrap();
        assert!(
            block.contains("- attempt 1: FAILURE_CLASS"),
            "earlier entries render by their first line:\n{block}"
        );
        assert!(
            !block.contains("- attempt 2:"),
            "the latest failure belongs to the detail section only, never to the earlier list:\n{block}"
        );
        assert_eq!(
            block.matches("FAILURE_SIGNATURE: no_changes").count(),
            1,
            "the latest failure appears once, as the detail:\n{block}"
        );
    }

    /// A single prior attempt gets no "earlier attempts" section — there is nothing earlier.
    #[test]
    fn a_single_prior_attempt_adds_no_earlier_section() {
        let prior = vec![format_error_feedback(&CoderError::NoChanges)];
        let block = repair_focus_block(&prior).unwrap();
        assert!(
            !block.contains("Earlier attempts"),
            "one attempt has no history to list:\n{block}"
        );
    }

    /// Signatures hash the message: two different failures cannot share one, or churn
    /// detection mistakes a new failure for a repeat.
    #[test]
    fn signatures_differ_between_different_messages() {
        let a = format_error_feedback(&CoderError::Backend("first failure".into()));
        let b = format_error_feedback(&CoderError::Backend("second failure".into()));
        let sig = |s: &str| {
            s.lines()
                .find(|l| l.starts_with("FAILURE_SIGNATURE:"))
                .unwrap_or_else(|| panic!("no signature in {s}"))
                .to_string()
        };
        let (sa, sb) = (sig(&a), sig(&b));
        assert_ne!(sa, sb);
        // A sha256 hex digest, not a placeholder.
        let hex = sa
            .split_once(": ")
            .map(|(_, h)| h)
            .unwrap_or_else(|| panic!("malformed signature line {sa}"));
        assert_eq!(hex.len(), 64, "{sa}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{sa}");
    }
}
