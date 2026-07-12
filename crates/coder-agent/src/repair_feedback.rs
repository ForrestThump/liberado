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
            Self::Other => "other",
        }
    }

    pub fn repair_hint(self) -> &'static str {
        match self {
            Self::NoChanges => {
                "Make a real workspace mutation (write_file/edit_file/apply_patch) that leaves a git diff."
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
            Self::ValidationOther | Self::Other => {
                "Read the findings carefully; change approach if the same signature already failed."
            }
        }
    }
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
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.chars().take(400).collect::<String>().hash(&mut h);
    format!("{:x}", h.finish())
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
}
