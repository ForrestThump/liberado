//! Coding-pack verifier pipeline: structural + command + git checks on a real workspace.
//!
//! Implements the harness half of `docs/architecture/verifiers.md`. Specs are DTOs from
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
    let excerpt = truncate_log(&format!(
        "stdout:\n{}\nstderr:\n{}",
        output.stdout, output.stderr
    ));
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
    match gates::changed_files(&root.to_string_lossy()).await {
        Ok(files) if !files.is_empty() => {
            Verdict::pass(format!("non-empty diff ({} paths)", files.len()))
        }
        Ok(_) => Verdict::fail(
            "empty git status (no workspace changes)",
            vec![Finding {
                check_id: id.to_string(),
                kind: FindingKind::EmptyDiff,
                message: "no files changed in workspace".into(),
                detail: None,
            }],
            None,
        ),
        Err(e) => Verdict::error(e.to_string()),
    }
}

fn truncate_log(s: &str) -> String {
    const MAX: usize = 4_000;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}…[truncated]", &s[..MAX])
    }
}

fn signature_pipeline(results: &[NamedVerdict]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    for r in results {
        r.id.hash(&mut h);
        r.verdict.signature.hash(&mut h);
        r.verdict.summary.hash(&mut h);
    }
    format!("{:x}", h.finish())
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
}
