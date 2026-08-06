//! Phase 2 of a coding session: **build** against the frozen contract.
//!
//! A bounded attempt loop. An attempt ends one of three ways and they are not the same thing: it
//! RAN (a verdict), it got STUCK (ask a human — being stuck is the strongest reason to interrupt
//! someone), or the environment BROKE (fail fast; no answer you can type fixes a dead sandbox).
//! Conflating those is what made the ask seam unreachable from the case that most needed it.

use liberado_coder_core::{
    CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, GoalContract,
    LIBERADO_LOOP_BACKEND, ProgressPolicy, SandboxSpec, WorkspaceRef,
};
use liberado_common::Outcome;
use liberado_session::{
    GoalResult, GoalSpec, InputChannel, PackContext, PackError, SessionEvent, SessionEventKind,
    TerminalKind,
};
use std::path::PathBuf;
use tokio::sync::mpsc::Sender;

use super::CodingSessionPack;
use super::policies::WorkspacePolicies;

/// Is this a failure a **human answer** could plausibly unblock?
///
/// `NoChanges` = the model could not make progress. `Validation` = it could not satisfy a gate.
/// Both are "I am stuck", which is exactly when a person is worth interrupting — and both used to
/// kill the session outright, because the ask seam only ever ran on the success path.
///
/// Everything else (`Setup`, `Sandbox`, `Provider`, `Tool`, `Backend`) is a broken *environment*.
/// No answer you could type fixes a dead sandbox or an unreachable provider, so those still fail
/// fast: paging a human for them would be noise, and the whole value of the ask is that it is rare.
fn is_stuck(e: &liberado_coder_core::CoderError) -> bool {
    use liberado_coder_core::CoderError;
    matches!(e, CoderError::NoChanges | CoderError::Validation(_))
}

/// Whether `dir` is inside a git working tree — **not** merely whether it is a repo *root*.
///
/// `dir.join(".git").exists()` only answers for a root. A workspace pointed at a subdirectory of an
/// existing checkout would look like "not a repo", and [`init_git_repo`] would then create a nested
/// repository plus a placeholder commit inside someone's project. `rev-parse` answers the question
/// actually being asked.
fn is_git_repo(dir: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy()])
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|o| o.status.success() && o.stdout.starts_with(b"true"))
        .unwrap_or(false)
}

/// Make a freshly-created session workspace its **own** git repo.
///
/// Without this, a workspace created under the daemon's data dir (`.liberado/…`, which is a
/// *relative* path and so usually sits inside the user's own checkout) is not a repo, and every git
/// command run there — `git status` for `files_changed`, the coder's `git_diff` tool, a
/// `git_nonempty_diff` verifier — silently resolves against the **enclosing** repo instead. The
/// session would then report, and be graded on, changes it never made.
///
/// Best-effort: a workspace that is already a repo (the dogfood case, where the caller passes a real
/// checkout) never reaches here, and a git failure just leaves things as they were.
fn init_git_repo(dir: &std::path::Path) {
    if is_git_repo(dir) {
        return;
    }
    let ok = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        tracing::warn!(
            dir = %dir.display(),
            "could not `git init` the session workspace — file-change reporting may be unreliable"
        );
        return;
    }
    // An empty repo has no HEAD commit, so `git worktree add` would fail.
    // Seed a placeholder commit so Worktree isolation can proceed.
    let placeholder = dir.join(".liberado-placeholder");
    let _ = std::fs::write(&placeholder, "");
    let _ = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy()])
        .args(["add", ".liberado-placeholder"])
        .status();
    let _ = std::process::Command::new("git")
        .args(["-C", &dir.to_string_lossy()])
        .args([
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "liberado workspace root",
        ])
        .env("GIT_AUTHOR_NAME", "liberado")
        .env("GIT_AUTHOR_EMAIL", "liberado@local")
        .env("GIT_COMMITTER_NAME", "liberado")
        .env("GIT_COMMITTER_EMAIL", "liberado@local")
        .status();
}

impl CodingSessionPack {
    /// Build against the frozen contract (or, for a session that could not ask, against the bare
    /// description). Bounded attempt loop; a failed attempt may spend one ask on a human and fold
    /// their answer into the next attempt's `prior_feedback`.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_build_phase(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        ctx: &PackContext<'_>,
        contract: Option<Box<GoalContract>>,
        events: Sender<SessionEvent>,
        mut inputs: InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        let may_ask = ctx.can(&liberado_common::Capability::AskHuman);

        // ── Phase 2: build against the frozen contract ──────────────────────────────────────
        let workspace = goal
            .payload
            .get("workspace_root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                self.default_workspace_parent
                    .join(format!("goal-{session_id}"))
            });

        let _ = std::fs::create_dir_all(&workspace);
        init_git_repo(&workspace);

        let model = goal
            .payload
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("session-coder")
            .to_string();

        // Path/command policy from profile overrides + payload (plan mode = restricted preset).
        let policies = WorkspacePolicies::resolve(ctx.overrides(), &goal.payload);
        let prompt = policies.coder_prompt(
            &goal.payload,
            "You are Liberado's coding worker. Inspect, edit with tools, then submit_report.",
        );

        let max_turns = if policies.plan_mode {
            // Plans are short; keep the bound tight so a looping planner cannot burn a full build
            // budget. Cap an explicit max_turns from a direct API call too.
            if goal.max_turns > 0 {
                8.min(goal.max_turns)
            } else {
                8
            }
        } else if goal.max_turns > 0 {
            goal.max_turns
        } else {
            12
        };

        let role = CoderRoleConfig {
            model: model.clone(),
            prompt_path: None,
            prompt: Some(prompt),
            temperature: Some(0.1),
            max_tokens: None,
            max_turns: Some(max_turns),
        };
        let disabled = CoderRoleConfig {
            model: model.clone(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: Some(2),
        };

        let mut task = CoderTask::new(session_id, &goal.description);
        task.success_criteria = goal.success_criteria.clone();

        let mut request = CoderRunRequest {
            task,
            workspace: WorkspaceRef::new(workspace.to_string_lossy(), "HEAD"),
            config: CoderRunConfig {
                backend: LIBERADO_LOOP_BACKEND.into(),
                trace_dir: None,
                planner: disabled.clone(),
                coder: role.clone(),
                critic: disabled,
                gate: liberado_coder_core::CoderGateConfig::default(),
                // Plan mode: no repair loop rewriting the workspace — one pass to the plan file.
                repair: if policies.plan_mode { None } else { Some(role) },
                sandbox: if is_git_repo(&workspace) {
                    SandboxSpec::Worktree
                } else {
                    SandboxSpec::HostLocal
                },
                command_policy: policies.command_policy.clone(),
                validation_command: None,
                verifiers: Vec::new(),
                verify_policy: Default::default(),
                path_policy: policies.path_policy.clone(),
                progress: ProgressPolicy {
                    max_attempts: if policies.plan_mode { 1 } else { 2 },
                    ..ProgressPolicy::default()
                },
            },
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        };

        if policies.plan_mode {
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::Progress {
                        message: format!(
                            "plan mode: writes limited to {}; shell disabled",
                            liberado_coder_core::PLAN_ARTIFACT_REL
                        ),
                    },
                ))
                .await;
        }

        // The frozen contract overwrites description, success criteria, and — the point of the
        // whole exercise — the **verifiers**. Without it these stay empty and the loop grades its
        // own homework; with it, the gates are the human's, stamped with a content hash the worker
        // cannot alter.
        if let Some(contract) = &contract {
            // The gates are only meaningful if they are the ones the human accepted. Prove that
            // before handing them to the worker they will grade it — a contract that no longer
            // matches its own hash is a broken promise, not a gate, so refuse rather than build
            // against it.
            contract
                .verify_integrity()
                .map_err(|e| PackError::Setup(format!("refusing to build: {e}")))?;
            contract.apply_to_request(&mut request);
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::Progress {
                        message: format!(
                            "contract frozen ({} verifier(s), hash {}) — building against it",
                            request.config.verifiers.len(),
                            // The hash is `<algo>:<digest>`; show the digest, not the algo prefix.
                            contract
                                .content_hash
                                .rsplit(':')
                                .next()
                                .unwrap_or(&contract.content_hash),
                        ),
                    },
                ))
                .await;
        }

        // E5: the build is a bounded attempt loop, not a single shot. When an attempt fails and this
        // session may ask a human, the pack stops and asks — and the answer comes back as a
        // `prior_feedback` line on the *next* attempt. That is the same channel the verifier repair
        // loop already uses, and the workspace still holds the failed attempt's changes, so the
        // retry continues from where it broke rather than redoing the work. Bounded by
        // `max_mid_run_asks` (default 1): a pack that can ask forever is a chat, not a pack.
        let mut asks_remaining = if may_ask {
            ctx.overrides()
                .get("max_mid_run_asks")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as u32
        } else {
            0
        };

        loop {
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::RoleStarted {
                        role: "coder".into(),
                        model: model.clone(),
                    },
                ))
                .await;

            // Race coding run against cancel (best-effort; LiberadoLoopBackend is not cancel-aware).
            use crate::completion_gate::LIVE_GATE;
            let run_fut = LIVE_GATE.scope(
                (events.clone(), session_id.to_string()),
                self.backend.run(request.clone()),
            );
            tokio::pin!(run_fut);

            let result = tokio::select! {
                r = &mut run_fut => r,
                _ = cancel.changed() => {
                    if *cancel.borrow() {
                        return Err(PackError::Cancelled);
                    }
                    run_fut.await
                }
            };

            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::RoleFinished {
                        role: "coder".into(),
                    },
                ))
                .await;

            // An attempt ends one of three ways, and conflating them is what broke this seam:
            //
            //  * it RAN and produced a verdict (`Ok`) — pass or fail;
            //  * it got STUCK (`NoChanges`, `Validation`) — the model could not make progress. This
            //    is the *strongest* reason to ask a human, and it used to be the one case that
            //    could not: the ask lived on the `Ok` path only, so the more stuck the pack got,
            //    the less able it was to ask. Found by the live test, where the coder built a
            //    working CLI, hit a gate it had no way to satisfy, and died silently instead of
            //    asking for the one thing only the human had;
            //  * it BROKE (`Setup`/`Sandbox`/`Provider`/`Tool`/`Backend`) — the environment failed.
            //    No human answer fixes a dead sandbox, so fail fast rather than page someone.
            let (ok, summary, artifacts, diagnostics) = match result {
                Ok(r) => {
                    let ok = r.outcome == Outcome::Succeeded;
                    // Completion-gate votes stream live via C2 (LIVE_GATE task-local);
                    // the file changes are the evidence they are about, so surface them first.
                    for change in &r.file_changes {
                        let _ = events
                            .send(SessionEvent::new(
                                session_id,
                                SessionEventKind::FileChanged {
                                    path: change.path.clone(),
                                    change: change.change.clone(),
                                },
                            ))
                            .await;
                    }
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::ValidationFinished {
                                ok,
                                summary: r
                                    .validation_notes
                                    .clone()
                                    .unwrap_or_else(|| r.summary.clone()),
                            },
                        ))
                        .await;
                    (ok, r.summary, r.files_changed, r.diagnostics)
                }
                Err(e) if is_stuck(&e) => {
                    let msg = e.to_string();
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::ValidationFinished {
                                ok: false,
                                summary: msg.clone(),
                            },
                        ))
                        .await;
                    (
                        false,
                        msg,
                        Vec::new(),
                        serde_json::json!({"error": "coder_backend", "stuck": true}),
                    )
                }
                Err(e) => {
                    let msg = e.to_string();
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::Failed {
                                message: msg.clone(),
                            },
                        ))
                        .await;
                    return Ok(GoalResult {
                        terminal: TerminalKind::Failed,
                        summary: msg,
                        artifacts: vec![],
                        diagnostics: serde_json::json!({"error": "coder_backend"}),
                    });
                }
            };

            // Succeeded, or failed with no ask left to spend: this is the outcome.
            if ok || asks_remaining == 0 {
                return Ok(GoalResult {
                    terminal: if ok {
                        TerminalKind::Succeeded
                    } else {
                        TerminalKind::Failed
                    },
                    summary,
                    artifacts,
                    diagnostics,
                });
            }

            let prompt = format!(
                "The build did not succeed:\n{}\n\nHow should I proceed? \
                 Reply with guidance, or \"abort\" to stop.",
                summary
            );
            let answer = self
                .ask(
                    session_id,
                    ctx,
                    &events,
                    &mut inputs,
                    &mut cancel,
                    prompt,
                    vec!["abort".into(), "retry".into()],
                )
                .await?;

            match answer {
                // Nobody answered inside the idle budget. The work stands; say so plainly.
                None => {
                    return Ok(GoalResult {
                        terminal: TerminalKind::BudgetExhausted,
                        summary: format!(
                            "build failed and no answer to mid-run question: {summary}"
                        ),
                        artifacts,
                        diagnostics,
                    });
                }
                Some(text)
                    if text.trim().eq_ignore_ascii_case("abort")
                        || text.trim().eq_ignore_ascii_case("stop")
                        || text.trim().eq_ignore_ascii_case("cancel") =>
                {
                    return Ok(GoalResult {
                        terminal: TerminalKind::Cancelled,
                        summary: format!("build failed; human aborted after: {summary}"),
                        artifacts,
                        diagnostics,
                    });
                }
                Some(guidance) => {
                    asks_remaining -= 1;
                    request.attempt += 1;
                    request.prior_feedback.push(format!(
                        "Attempt {} failed: {summary}\nHuman guidance: {guidance}",
                        request.attempt
                    ));
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::Progress {
                                message: format!(
                                    "retrying with human guidance: {}",
                                    guidance.chars().take(120).collect::<String>()
                                ),
                            },
                        ))
                        .await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A **subdirectory** of a checkout is inside a work tree, so nothing must be initialised
    /// there. The previous check was `dir.join(".git").exists()`, which only answers for a repo
    /// root — pointing a session at `repo/crates/foo` would have created a nested repository and a
    /// placeholder commit inside someone's project.
    #[test]
    fn a_subdirectory_of_a_repo_is_already_a_repo_and_is_not_reinitialised() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let sub = dir.path().join("crates").join("thing");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(
            is_git_repo(&sub),
            "a subdirectory of a checkout is inside the work tree"
        );

        init_git_repo(&sub);
        assert!(
            !sub.join(".git").exists(),
            "init must not create a nested repository inside an existing checkout"
        );
        assert!(
            !sub.join(".liberado-placeholder").exists(),
            "and must not drop a placeholder commit into someone's project"
        );
    }

    #[test]
    fn is_git_repo_returns_false_for_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_git_repo(dir.path()));
    }

    #[test]
    fn is_git_repo_returns_true_after_init() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert!(is_git_repo(dir.path()));
    }

    #[test]
    fn init_git_repo_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        assert!(is_git_repo(dir.path()));
        init_git_repo(dir.path());
        assert!(is_git_repo(dir.path()));
    }

    #[test]
    fn init_git_repo_creates_a_commit_so_worktree_can_proceed() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        let output = std::process::Command::new("git")
            .args(["-C", &dir.path().to_string_lossy()])
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "init_git_repo must leave at least one commit so WorktreeWorkspace can proceed"
        );
    }
}
