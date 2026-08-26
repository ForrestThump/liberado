//! Task acceptance preflight (plan §15 D3): the delegator's `acceptance.preflight`
//! steps run in the worktree after the pack succeeds and before anything is pushed.
//!
//! The machinery is the pack's own — [`liberado_coder_sandbox::preflight`] runs shell
//! steps with timeouts, log caps, and per-step required flags; this file only maps the
//! wire DTO onto it and decides what a failure means. A required failure fails the
//! task honestly (nothing reaches the forge); an optional failure rides along as a
//! note the PR body must carry.

use liberado_coder_sandbox::{PreflightSpec, PreflightStep};
use liberado_delegate_contract::PreflightStepDto;

/// Outcome of enforcing the spec's acceptance steps.
#[derive(Debug)]
pub struct PreflightOutcome {
    /// Human-readable section for the PR body; empty when no steps were declared.
    pub body_section: String,
}

/// Run the steps and enforce them. Empty specs short-circuit: most tasks carry none.
pub(super) async fn enforce(
    worktree: &std::path::Path,
    steps: &[PreflightStepDto],
) -> Result<PreflightOutcome, String> {
    if steps.is_empty() {
        return Ok(PreflightOutcome {
            body_section: String::new(),
        });
    }
    let mapped: Vec<PreflightStep> = steps
        .iter()
        .map(|dto| {
            let step = PreflightStep::new(&dto.name, &dto.run);
            match dto.timeout_secs {
                Some(secs) => step.with_timeout_secs(secs),
                None => step,
            }
        })
        .collect();
    let report = liberado_coder_sandbox::run_preflight(
        worktree,
        &PreflightSpec::new("task-acceptance", mapped),
    )
    .await
    .map_err(|error| format!("run acceptance preflight: {error}"))?;

    let mut lines = vec!["## Acceptance preflight".to_string(), String::new()];
    let mut failed_required = Vec::new();
    for result in &report.steps {
        let mark = if result.ok { "pass" } else { "FAIL" };
        lines.push(format!(
            "- [{mark}] `{}` (exit {:?}, {}ms)",
            result.name, result.exit_code, result.duration_ms
        ));
        if !result.ok {
            let required = steps
                .iter()
                .find(|s| s.name == result.name)
                .map(|s| s.required)
                .unwrap_or(true);
            if required {
                failed_required.push(format!(
                    "`{}`: {}",
                    result.name,
                    tail_excerpt(&result.log_excerpt)
                ));
            } else {
                lines.push(format!(
                    "  optional failure ignored: {}",
                    tail_excerpt(&result.log_excerpt)
                ));
            }
        }
    }

    if !failed_required.is_empty() {
        return Err(format!(
            "acceptance preflight failed (required): {}",
            failed_required.join("; ")
        ));
    }
    Ok(PreflightOutcome {
        body_section: lines.join("\n"),
    })
}

fn tail_excerpt(log: &str) -> String {
    let trimmed = log.trim();
    if trimmed.is_empty() {
        return "no output".into();
    }
    let last = trimmed.lines().last().unwrap_or(trimmed);
    if last.chars().count() > 160 {
        format!("{}…", last.chars().take(159).collect::<String>())
    } else {
        last.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, run: &str, required: bool) -> PreflightStepDto {
        PreflightStepDto {
            name: name.into(),
            run: run.into(),
            timeout_secs: Some(10),
            required,
        }
    }

    #[tokio::test]
    async fn empty_specs_short_circuit_without_touching_the_worktree() {
        let outcome = enforce(std::path::Path::new("/definitely/not/here"), &[]).await;
        assert!(outcome.is_ok(), "no steps means nothing to run");
        assert!(outcome.unwrap().body_section.is_empty());
    }

    #[tokio::test]
    async fn passing_steps_land_in_the_body_section() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker.txt"), "x").unwrap();
        let outcome = enforce(
            dir.path(),
            &[step("has-marker", "test -f marker.txt", true)],
        )
        .await
        .unwrap();
        assert!(
            outcome.body_section.contains("[pass]"),
            "{}",
            outcome.body_section
        );
        assert!(
            outcome.body_section.contains("has-marker"),
            "{}",
            outcome.body_section
        );
    }

    /// The D3 gate: a deliberately failing *required* step stops the deliverable.
    #[tokio::test]
    async fn a_failing_required_step_refuses_the_run_with_the_step_named() {
        let dir = tempfile::tempdir().unwrap();
        let error = enforce(dir.path(), &[step("always-red", "false", true)])
            .await
            .expect_err("required red must fail");
        assert!(error.contains("always-red"), "{error}");
        assert!(error.contains("required"), "{error}");
    }

    #[tokio::test]
    async fn an_optional_failure_is_recorded_but_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ok.txt"), "x").unwrap();
        let outcome = enforce(
            dir.path(),
            &[
                step("soft-fail", "false", false),
                step("hard-pass", "test -f ok.txt", true),
            ],
        )
        .await
        .unwrap();
        assert!(
            outcome.body_section.contains("[FAIL]"),
            "{}",
            outcome.body_section
        );
        assert!(outcome.body_section.contains("optional failure ignored"));
    }

    #[tokio::test]
    async fn timeouts_count_as_failures() {
        let dir = tempfile::tempdir().unwrap();
        let error = enforce(
            dir.path(),
            &[PreflightStepDto {
                name: "hangs".into(),
                run: "sleep 30".into(),
                timeout_secs: Some(1),
                required: true,
            }],
        )
        .await
        .expect_err("timeout must fail");
        assert!(error.contains("hangs"), "{error}");
    }
}
