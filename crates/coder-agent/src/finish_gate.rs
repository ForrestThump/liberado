//! Same-session compile check on `submit_report outcome=succeeded`.
//!
//! The post-execute ship bar used to be the first compile check. By then the executor
//! conversation was over, so a red `cargo check` became a new attempt with only
//! `prior_feedback`. The files stayed — but the model that wrote them did not.
//!
//! This gate runs *inside* that conversation. A red check is a tool result, not a new
//! loop. Partial, Failed, Proposed, wrap-up, and turn exhaustion never reach it: they
//! already tell the truth, and they do not revert the worktree.

use async_trait::async_trait;
use liberado_coder_core::{PipelinePolicy, VerifierSpec};
use liberado_common::{Outcome, Report};
use liberado_executor::ReportGate;

/// Refuse a live `succeeded` until `cargo check` is green. Never touches the worktree.
pub struct WorkspaceCompileGate {
    workspace_root: String,
}

impl WorkspaceCompileGate {
    pub fn new(workspace_root: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }
}

/// `true` when the report may end the loop without a compile check.
///
/// Same rule as [`liberado_executor::Executor::report_ends_without_gate`]: wrap-up and every
/// outcome except `succeeded` keep the files and the report. The kernel already skips the
/// gate in those cases; this predicate is the pack-side copy so unit tests can pin the
/// decision without driving the loop.
pub fn accept_without_compile_check(outcome: Outcome, wrapping_up: bool) -> bool {
    wrapping_up || !matches!(outcome, Outcome::Succeeded)
}

fn cargo_check_spec() -> VerifierSpec {
    VerifierSpec::Command {
        id: "cargo-check".into(),
        program: "cargo".into(),
        args: vec!["check".into(), "--workspace".into(), "--all-targets".into()],
        env: Default::default(),
        timeout_secs: Some(900),
        output_max_bytes: None,
        network: false,
    }
}

fn refused_succeeded(body: &str) -> String {
    format!(
        "`submit_report` with outcome=succeeded was NOT accepted — `cargo check` is red. \
         Your files are still on disk; nothing was reverted. Fix the compile errors in \
         this session, then submit succeeded only when check is green. If you cannot \
         finish, submit outcome=partially_succeeded or failed.\n\n{body}"
    )
}

/// Host failed (disk, OOM). The model cannot repair this; do not ask it to.
fn refused_host_failure(body: &str) -> String {
    format!(
        "`submit_report` with outcome=succeeded was NOT accepted — the host failed, \
         not the change. Your files are still on disk. Do not try to repair this. \
         An operator must act.\n\n{body}"
    )
}

#[async_trait]
impl ReportGate for WorkspaceCompileGate {
    async fn accept(&self, report: &Report, wrapping_up: bool) -> Result<(), String> {
        if accept_without_compile_check(report.outcome, wrapping_up) {
            return Ok(());
        }
        let root = std::path::Path::new(&self.workspace_root);
        if !root.join("Cargo.toml").exists() {
            return Ok(());
        }
        let pipeline = crate::verify_pipeline::run_pipeline(
            &self.workspace_root,
            &[cargo_check_spec()],
            &PipelinePolicy::default(),
            None,
        )
        .await
        .map_err(|e| {
            format!(
                "`submit_report` with outcome=succeeded was NOT accepted — the workspace \
                 compile check could not run ({e}). Your files are still on disk. Fix the \
                 environment or call submit_report with outcome=partially_succeeded / failed."
            )
        })?;
        if pipeline.is_pass() {
            return Ok(());
        }
        let body = crate::repair_feedback::format_pipeline_repair(&pipeline);
        if crate::repair_feedback::classify_pipeline(&pipeline)
            == crate::repair_feedback::FailureClass::Infrastructure
        {
            return Err(refused_host_failure(&body));
        }
        Err(refused_succeeded(&body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(outcome: Outcome) -> Report {
        Report {
            outcome,
            summary: "done".into(),
            artifacts: Vec::new(),
            new_high_signal_facts: Vec::new(),
            follow_up: None,
            deferred_to_human: false,
            repeat_calls: 0,
        }
    }

    #[test]
    fn honest_terminals_skip_the_compile_check() {
        assert!(accept_without_compile_check(
            Outcome::PartiallySucceeded,
            false
        ));
        assert!(accept_without_compile_check(Outcome::Failed, false));
        assert!(accept_without_compile_check(Outcome::Proposed, false));
        assert!(accept_without_compile_check(Outcome::Succeeded, true));
        assert!(!accept_without_compile_check(Outcome::Succeeded, false));
    }

    #[tokio::test]
    async fn no_cargo_toml_accepts_succeeded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gate = WorkspaceCompileGate::new(dir.path().to_string_lossy());
        gate.accept(&report(Outcome::Succeeded), false)
            .await
            .expect("a non-Rust workspace has no compile gate");
    }

    #[tokio::test]
    async fn wrap_up_and_partial_accept_without_looking_at_the_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        // A Cargo.toml that would fail if we ran check — the point is we must not.
        std::fs::write(dir.path().join("Cargo.toml"), "this is not a manifest")
            .expect("write decoy Cargo.toml");
        let gate = WorkspaceCompileGate::new(dir.path().to_string_lossy());
        gate.accept(&report(Outcome::PartiallySucceeded), false)
            .await
            .expect("partial keeps the files and the report");
        gate.accept(&report(Outcome::Failed), false)
            .await
            .expect("failed keeps the files and the report");
        gate.accept(&report(Outcome::Succeeded), true)
            .await
            .expect("wrap-up must not run check or we trap a model that cannot edit");
    }

    #[test]
    fn refuse_message_says_the_files_stay() {
        let shown = refused_succeeded("FINDINGS:\n- compile error");
        assert!(
            shown.contains("still on disk"),
            "the model must be told the work was kept: {shown}"
        );
        assert!(
            shown.contains("NOT accepted"),
            "the model must be told succeeded was refused: {shown}"
        );
        assert!(
            shown.contains("partially_succeeded"),
            "the model must be told how to leave with half-finished work: {shown}"
        );
    }

    #[test]
    fn host_failure_does_not_ask_the_model_to_fix_compile_errors() {
        let shown = refused_host_failure("FAILURE_CLASS: infrastructure\nno space on device");
        assert!(shown.contains("host failed"), "{shown}");
        assert!(shown.contains("operator"), "{shown}");
        assert!(
            !shown.contains("Fix the compile errors"),
            "a full disk is not a red crate: {shown}"
        );
    }
}
