//! Coding-pack verifier pipeline: structural + command + git checks on a real workspace.
//!
//! Implements the harness half of `docs/spec/architecture/verifiers.md`. Specs are DTOs from
//! `liberado-coder-core`; this module executes them and never trusts the model report alone.

use std::path::{Path, PathBuf};

use chrono::Utc;
use liberado_coder_core::CommandPolicy;
use liberado_coder_core::{
    CoderError, CoderEvent, Finding, FindingKind, NamedVerdict, PipelinePolicy, PipelineResult,
    Verdict, VerdictStatus, VerifierSpec,
};
use liberado_coder_sandbox::{CommandOutput, CommandRequest, CommandRunner, HostWorkspace};

use crate::gates;
use crate::trace::{self, EventLog};

/// Run resolved specs against `workspace_root`. Emits per-check validation events when `events` set.
pub async fn run_pipeline(
    workspace_root: &str,
    specs: &[VerifierSpec],
    policy: &PipelinePolicy,
    events: Option<&EventLog>,
) -> Result<PipelineResult, CoderError> {
    let root = PathBuf::from(workspace_root);
    if !root.is_dir() {
        return Err(CoderError::Setup(format!(
            "workspace root is not a directory: {workspace_root}"
        )));
    }

    let mut results = Vec::new();
    let mut combined_findings = Vec::new();
    let mut overall = VerdictStatus::Pass;

    for spec in specs {
        let verdict = run_one(&root, spec).await;
        if let Some(events) = events {
            trace::push_event(
                events,
                CoderEvent::ValidationFinished {
                    ok: verdict.is_pass(),
                    summary: format!("{}: {}", spec.id(), verdict.summary),
                    at: Utc::now(),
                },
            );
        }

        let status = verdict.status;
        if !verdict.is_pass() {
            overall = if status == VerdictStatus::Error && !policy.errors_are_failures {
                overall
            } else {
                VerdictStatus::Fail
            };
            combined_findings.extend(verdict.findings.iter().cloned());
        }

        results.push(NamedVerdict {
            id: spec.id().to_string(),
            kind: spec.kind().to_string(),
            verdict,
        });

        if policy.fail_fast && overall == VerdictStatus::Fail {
            break;
        }
    }

    let combined_signature = if overall == VerdictStatus::Pass {
        None
    } else {
        Some(signature_pipeline(&results))
    };

    Ok(PipelineResult {
        overall,
        results,
        combined_findings,
        combined_signature,
    })
}

async fn run_one(root: &Path, spec: &VerifierSpec) -> Verdict {
    match spec {
        VerifierSpec::PathsExist { id, paths } => paths_exist(root, id, paths),
        VerifierSpec::PathsAbsent { id, paths } => paths_absent(root, id, paths),
        VerifierSpec::ContentContains {
            id,
            path,
            must_include,
        } => content_contains(root, id, path, must_include),
        VerifierSpec::Command {
            id,
            program,
            args,
            env,
            timeout_secs,
            output_max_bytes,
            network: _,
        } => {
            run_command_check(
                root,
                id,
                program,
                args,
                env,
                *timeout_secs,
                *output_max_bytes,
            )
            .await
        }
        VerifierSpec::GitNonemptyDiff { id } => git_nonempty_diff(root, id).await,
    }
}

fn paths_exist(root: &Path, id: &str, paths: &[String]) -> Verdict {
    let mut findings = Vec::new();
    for rel in paths {
        let p = root.join(rel);
        if !p.exists() {
            findings.push(Finding {
                check_id: id.to_string(),
                kind: FindingKind::MissingPath,
                message: format!("missing path: {rel}"),
                detail: None,
            });
        }
    }
    if findings.is_empty() {
        Verdict::pass(format!("all {} paths exist", paths.len()))
    } else {
        Verdict::fail(
            format!("{} missing path(s)", findings.len()),
            findings,
            None,
        )
    }
}

fn paths_absent(root: &Path, id: &str, paths: &[String]) -> Verdict {
    let mut findings = Vec::new();
    for rel in paths {
        if root.join(rel).exists() {
            findings.push(Finding {
                check_id: id.to_string(),
                kind: FindingKind::UnexpectedChange,
                message: format!("path should not exist: {rel}"),
                detail: None,
            });
        }
    }
    if findings.is_empty() {
        Verdict::pass("forbidden paths absent")
    } else {
        Verdict::fail(
            format!("{} unexpected path(s)", findings.len()),
            findings,
            None,
        )
    }
}

fn content_contains(root: &Path, id: &str, path: &str, must_include: &[String]) -> Verdict {
    let full = root.join(path);
    let body = match std::fs::read_to_string(&full) {
        Ok(b) => b,
        Err(e) => {
            return Verdict::fail(
                format!("cannot read {path}: {e}"),
                vec![Finding {
                    check_id: id.to_string(),
                    kind: FindingKind::MissingPath,
                    message: format!("cannot read {path}: {e}"),
                    detail: None,
                }],
                None,
            );
        }
    };
    let mut findings = Vec::new();
    for needle in must_include {
        if !body.contains(needle) {
            findings.push(Finding {
                check_id: id.to_string(),
                kind: FindingKind::ContentMismatch,
                message: format!("{path} must contain {needle:?}"),
                detail: None,
            });
        }
    }
    if findings.is_empty() {
        Verdict::pass(format!("{path} content ok"))
    } else {
        Verdict::fail(
            format!("{} content mismatch(es) in {path}", findings.len()),
            findings,
            None,
        )
    }
}

async fn run_command_check(
    root: &Path,
    id: &str,
    program: &str,
    args: &[String],
    env: &std::collections::BTreeMap<String, String>,
    timeout_secs: Option<u64>,
    output_max_bytes: Option<usize>,
) -> Verdict {
    // Backend gate: run outside model command allowlist, still with timeout/caps.
    let req = CommandRequest {
        program: program.to_string(),
        args: args.to_vec(),
        env: env.clone(),
        timeout_secs,
        output_max_bytes,
        // Backend ship bar, not a model tool result: keep head truncation.
        offload_dir: None,
    };
    // Prefer HostWorkspace runner for consistent caps when available.
    // Empty allow list = all programs permitted (backend gate is not the model allowlist).
    let ws = match HostWorkspace::new(root, CommandPolicy::default()) {
        Ok(w) => w,
        Err(e) => return Verdict::error(format!("workspace: {e}")),
    };
    let output = match ws.run_command(req).await {
        Ok(o) => o,
        Err(e) => {
            return Verdict::fail(
                format!("command spawn/run failed: {e}"),
                vec![Finding {
                    check_id: id.to_string(),
                    kind: FindingKind::CommandFailed,
                    message: e.to_string(),
                    detail: None,
                }],
                None,
            );
        }
    };
    command_output_to_verdict(id, program, &output)
}

fn command_output_to_verdict(id: &str, program: &str, output: &CommandOutput) -> Verdict {
    let combined = format!("stdout:\n{}\nstderr:\n{}", output.stdout, output.stderr);
    // Select diagnostics while the whole captured command result is still available. A later
    // byte cap cannot recover the failing package after a long passing workspace-test tail.
    let excerpt = truncate_log(&crate::repair_feedback::clip_log_excerpt(&combined, 80));
    if output.timed_out {
        return Verdict::fail(
            format!("{program} timed out"),
            vec![Finding {
                check_id: id.to_string(),
                kind: FindingKind::CommandTimeout,
                message: format!("{program} timed out"),
                detail: None,
            }],
            Some(excerpt),
        );
    }
    let code = output.exit_code.unwrap_or(-1);
    if code == 0 {
        Verdict::pass(format!("{program} exited 0"))
    } else {
        Verdict::fail(
            format!("{program} exited {code}"),
            vec![Finding {
                check_id: id.to_string(),
                kind: FindingKind::CommandFailed,
                message: format!("{program} exited {code}"),
                detail: Some(serde_json::json!({ "exit_code": code })),
            }],
            Some(excerpt),
        )
    }
}

async fn git_nonempty_diff(root: &Path, id: &str) -> Verdict {
    let root_str = root.to_string_lossy();
    // Check uncommitted changes first (git status), then committed changes
    // (git log -1) — covers git-merge+commit workflows where the tree is clean.
    match gates::changed_files(&root_str).await {
        Ok(files) if !files.is_empty() => {
            return Verdict::pass(format!(
                "non-empty diff ({} uncommitted paths)",
                files.len()
            ));
        }
        Err(e) => return Verdict::error(e.to_string()),
        _ => {}
    }
    // No uncommitted changes — check the most recent commit.
    match changed_files_in_last_commit(&root_str).await {
        Ok(files) if !files.is_empty() => {
            return Verdict::pass(format!(
                "non-empty diff ({} paths in last commit)",
                files.len()
            ));
        }
        Err(e) => return Verdict::error(e.to_string()),
        _ => {}
    }
    Verdict::fail(
        "empty diff (no workspace or committed changes)",
        vec![Finding {
            check_id: id.to_string(),
            kind: FindingKind::EmptyDiff,
            message: "no files changed in workspace or last commit".into(),
            detail: None,
        }],
        None,
    )
}

async fn changed_files_in_last_commit(workspace_root: &str) -> Result<Vec<String>, CoderError> {
    let output = liberado_common::process::command("git")
        .args(["log", "-1", "--name-only", "--format="])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git log: {e}")))?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

fn truncate_log(s: &str) -> String {
    const MAX: usize = 4_000;
    if s.len() <= MAX {
        s.to_string()
    } else {
        const MARKER: &str = "\n…[truncated]";
        let content_max = MAX.saturating_sub(MARKER.len());
        format!("{}{}", prefix_at_char_boundary(s, content_max), MARKER)
    }
}

fn prefix_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn signature_pipeline(results: &[NamedVerdict]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for r in results {
        h.update(r.id.as_bytes());
        h.update(r.verdict.signature.as_deref().unwrap_or("").as_bytes());
        h.update(r.verdict.summary.as_bytes());
    }
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::VerifierSpec;

    #[tokio::test]
    async fn paths_exist_and_content_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello from liberado\n").unwrap();
        let specs = vec![
            VerifierSpec::PathsExist {
                id: "p".into(),
                paths: vec!["hello.txt".into()],
            },
            VerifierSpec::ContentContains {
                id: "c".into(),
                path: "hello.txt".into(),
                must_include: vec!["hello from liberado".into()],
            },
        ];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert!(result.is_pass());
        assert!(
            result.combined_signature.is_none(),
            "a passing pipeline must not produce a combined signature"
        );
    }

    #[tokio::test]
    async fn missing_path_fails_with_feedback() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![VerifierSpec::PathsExist {
            id: "p".into(),
            paths: vec!["nope.rs".into()],
        }];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert!(!result.is_pass());
        assert!(result.repair_feedback().contains("nope.rs"));
    }

    /// Compare 4/9 shape: the workspace failure is followed by enough passing `wire` output to
    /// defeat both the old first-4-KiB cap and the old last-40-lines repair excerpt.
    #[test]
    fn long_workspace_test_output_names_the_failing_package_in_repair_feedback() {
        let mut stdout = String::from(
            "running 1 test\ntest checkpoint::tests::resumes_cleanly ... FAILED\n\
             test result: FAILED. 0 passed; 1 failed; 0 ignored\n",
        );
        for n in 0..120 {
            stdout.push_str(&format!("test wire::tests::passing_case_{n:03} ... ok\n"));
        }
        stdout.push_str("test result: ok. 120 passed; 0 failed; 0 ignored\n");
        let output = CommandOutput {
            exit_code: Some(101),
            stdout,
            stderr: "error: test failed, to rerun pass `-p liberado-coder-sandbox --lib`\n".into(),
            timed_out: false,
            stdout_offload: None,
            stderr_offload: None,
        };

        let verdict = command_output_to_verdict("cargo-test", "cargo", &output);
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            combined_findings: verdict.findings.clone(),
            results: vec![NamedVerdict {
                id: "cargo-test".into(),
                kind: "command".into(),
                verdict,
            }],
            combined_signature: Some("compare-4-9".into()),
        };
        let feedback = crate::repair_feedback::format_pipeline_repair(&pipeline);

        assert!(
            feedback.contains("liberado-coder-sandbox"),
            "repair feedback must name the failing package: {feedback}"
        );
        assert!(
            feedback.contains("resumes_cleanly ... FAILED"),
            "repair feedback must retain the failing test: {feedback}"
        );
        assert!(
            feedback.lines().count() <= 50,
            "repair feedback must stay bounded: {} lines",
            feedback.lines().count()
        );
    }

    #[test]
    fn log_truncation_is_utf8_safe_and_bounded() {
        let input = format!("HEAD-{}🎉{}-TAIL", "a".repeat(3_990), "b".repeat(3_990));
        let clipped = truncate_log(&input);
        assert!(clipped.starts_with("HEAD-"), "{clipped}");
        assert!(clipped.ends_with("…[truncated]"), "{clipped}");
        assert!(clipped.len() <= 4_000, "{} bytes", clipped.len());
    }

    #[test]
    fn short_log_is_left_untouched() {
        let input = "short line";
        assert_eq!(truncate_log(input), input);
    }

    #[test]
    fn prefix_splits_at_char_boundary() {
        // "🎉" is 4 bytes; slicing at an interior byte must not panic.
        let s = "a🎉b";
        let p = prefix_at_char_boundary(s, 3);
        assert!(s.starts_with(p));
        assert!(s.is_char_boundary(p.len()));
        // The backward scan must not overshoot the byte budget.
        assert_eq!(p, "a");
        assert!(p.len() <= 3, "{p:?}");
    }

    #[tokio::test]
    async fn absent_paths_pass_when_forbidden_files_are_missing() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![VerifierSpec::PathsAbsent {
            id: "forbid".into(),
            paths: vec!["should-not-exist.rs".into()],
        }];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert!(result.is_pass(), "{:?}", result.overall);
    }

    #[tokio::test]
    async fn absent_paths_fail_when_forbidden_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("forbidden.rs"), "x").unwrap();
        let specs = vec![VerifierSpec::PathsAbsent {
            id: "forbid".into(),
            paths: vec!["forbidden.rs".into()],
        }];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert!(!result.is_pass());
        assert!(
            result
                .combined_findings
                .iter()
                .any(|f| f.kind == FindingKind::UnexpectedChange),
            "{:?}",
            result.combined_findings
        );
    }

    #[tokio::test]
    async fn content_check_fails_when_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![VerifierSpec::ContentContains {
            id: "c".into(),
            path: "nope.txt".into(),
            must_include: vec!["anything".into()],
        }];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert!(!result.is_pass());
        assert!(
            result
                .combined_findings
                .iter()
                .any(|f| f.kind == FindingKind::MissingPath),
            "{:?}",
            result.combined_findings
        );
    }

    #[tokio::test]
    async fn content_check_fails_when_needle_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha beta\n").unwrap();
        let specs = vec![VerifierSpec::ContentContains {
            id: "c".into(),
            path: "a.txt".into(),
            must_include: vec!["gamma".into()],
        }];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert!(!result.is_pass());
        assert!(
            result
                .combined_findings
                .iter()
                .any(|f| f.kind == FindingKind::ContentMismatch),
            "{:?}",
            result.combined_findings
        );
    }

    #[tokio::test]
    async fn workspace_must_be_a_directory() {
        let missing = tempfile::tempdir().unwrap().path().join("no-such-dir");
        let result = run_pipeline(
            &missing.to_string_lossy(),
            &[],
            &PipelinePolicy::default(),
            None,
        )
        .await;
        assert!(
            result.is_err(),
            "expected Setup error for non-directory root"
        );
    }

    #[test]
    fn command_without_exit_code_is_treated_as_failure() {
        let output = CommandOutput {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: false,
            stdout_offload: None,
            stderr_offload: None,
        };
        let verdict = command_output_to_verdict("probe", "probe", &output);
        assert!(!verdict.is_pass());
        assert!(
            verdict.summary.contains("exited -1"),
            "{:?}",
            verdict.summary
        );
    }

    #[test]
    fn signature_pipeline_is_deterministic_sha256_hex() {
        let mk = || NamedVerdict {
            id: "check".into(),
            kind: "paths".into(),
            verdict: Verdict::pass("ok"),
        };
        let s = signature_pipeline(&[mk(), mk()]);
        assert_eq!(s.len(), 64, "{s}");
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()), "{s}");
        assert_ne!(s, "");
        assert_ne!(s, "xyzzy");
        assert_eq!(
            signature_pipeline(&[mk(), mk()]),
            s,
            "must be deterministic"
        );
    }

    #[tokio::test]
    async fn error_verdict_is_not_failure_when_errors_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }];
        let policy = PipelinePolicy {
            errors_are_failures: false,
            ..PipelinePolicy::default()
        };
        let result = run_pipeline(&dir.path().to_string_lossy(), &specs, &policy, None)
            .await
            .unwrap();
        assert_eq!(result.overall, VerdictStatus::Pass, "{:?}", result.overall);
    }

    #[tokio::test]
    async fn error_verdict_is_failure_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let specs = vec![VerifierSpec::GitNonemptyDiff { id: "diff".into() }];
        let result = run_pipeline(
            &dir.path().to_string_lossy(),
            &specs,
            &PipelinePolicy::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.overall, VerdictStatus::Fail, "{:?}", result.overall);
    }

    #[tokio::test]
    async fn fail_fast_does_not_stop_when_all_specs_pass() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        let specs = vec![
            VerifierSpec::PathsExist {
                id: "a".into(),
                paths: vec!["a.txt".into()],
            },
            VerifierSpec::PathsExist {
                id: "b".into(),
                paths: vec!["b.txt".into()],
            },
        ];
        let policy = PipelinePolicy {
            fail_fast: true,
            ..PipelinePolicy::default()
        };
        let result = run_pipeline(&dir.path().to_string_lossy(), &specs, &policy, None)
            .await
            .unwrap();
        assert_eq!(
            result.results.len(),
            2,
            "fail_fast must not stop while specs keep passing"
        );
        assert!(result.is_pass());
    }

    fn init_git_only(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        for args in [
            ["init", "--quiet"].as_slice(),
            ["config", "user.email", "test@liberado.local"].as_slice(),
            ["config", "user.name", "test"].as_slice(),
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    fn init_repo(dir: &std::path::Path) {
        init_git_only(dir);
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["add", "README.md"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            std::process::Command::new("git")
                .args(["commit", "-m", "base", "--quiet"])
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }

    #[tokio::test]
    async fn git_nonempty_diff_passes_on_uncommitted_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_git_only(dir.path());
        std::fs::write(dir.path().join("new.rs"), "fn main(){}\n").unwrap();
        let verdict = git_nonempty_diff(dir.path(), "diff").await;
        assert!(verdict.is_pass(), "{:?}", verdict.summary);
    }

    #[tokio::test]
    async fn git_nonempty_diff_fails_on_an_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_git_only(dir.path());
        let verdict = git_nonempty_diff(dir.path(), "diff").await;
        assert_eq!(verdict.status, VerdictStatus::Fail, "{:?}", verdict.summary);
    }

    #[tokio::test]
    async fn git_nonempty_diff_passes_on_last_commit_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let verdict = git_nonempty_diff(dir.path(), "diff").await;
        assert!(verdict.is_pass(), "{:?}", verdict.summary);
    }

    #[tokio::test]
    async fn git_nonempty_diff_errors_outside_repo() {
        let dir = tempfile::tempdir().unwrap();
        let verdict = git_nonempty_diff(dir.path(), "diff").await;
        assert_eq!(
            verdict.status,
            VerdictStatus::Error,
            "{:?}",
            verdict.summary
        );
    }

    #[tokio::test]
    async fn last_commit_returns_committed_paths_and_filters_blank_lines() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let files = changed_files_in_last_commit(&dir.path().to_string_lossy())
            .await
            .unwrap();
        assert_eq!(files, vec!["README.md"], "{files:?}");
    }

    #[tokio::test]
    async fn last_commit_is_empty_when_git_fails() {
        let dir = tempfile::tempdir().unwrap();
        let files = changed_files_in_last_commit(&dir.path().to_string_lossy())
            .await
            .unwrap();
        assert!(files.is_empty());
    }
}

#[cfg(test)]
#[path = "verify_pipeline_survivor_tests.rs"]
mod survivor_tests;
