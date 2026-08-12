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
                "Reproduce the failing command locally with tools, fix the root cause, re-run until green."
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
    lines.push("Fix these before claiming success. Prefer a different approach if this signature already failed.".into());
    lines.join("\n")
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

pub fn classify_message(msg: &str) -> FailureClass {
    let lower = msg.to_ascii_lowercase();
    if lower.contains("failure_class:") {
        // Before the rest: the message also quotes the findings, so a disk-full run carries both
        // `infrastructure` and the `CommandFailed` text that produced it.
        if lower.contains("infrastructure") {
            return FailureClass::Infrastructure;
        }
        if lower.contains("no_changes") {
            return FailureClass::NoChanges;
        }
        if lower.contains("missing_path") {
            return FailureClass::MissingPath;
        }
        if lower.contains("content_mismatch") {
            return FailureClass::ContentMismatch;
        }
        if lower.contains("command_timeout") {
            return FailureClass::CommandTimeout;
        }
        if lower.contains("command_failed") {
            return FailureClass::CommandFailed;
        }
        if lower.contains("empty_diff") {
            return FailureClass::EmptyDiff;
        }
        if lower.contains("critic_revision") {
            return FailureClass::CriticRevision;
        }
    }
    if lower.contains("no real workspace changes") || lower.contains("no_changes") {
        return FailureClass::NoChanges;
    }
    if lower.contains("missing path") || lower.contains("missing_path") {
        return FailureClass::MissingPath;
    }
    if lower.contains("must contain") || lower.contains("content") {
        return FailureClass::ContentMismatch;
    }
    if lower.contains("timed out") {
        return FailureClass::CommandTimeout;
    }
    if lower.contains("exited") || lower.contains("command") {
        return FailureClass::CommandFailed;
    }
    if lower.contains("critic") {
        return FailureClass::CriticRevision;
    }
    if lower.contains("completeness") || lower.contains("validation") {
        return FailureClass::ValidationOther;
    }
    FailureClass::Other
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
}
