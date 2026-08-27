//! Phase 2 of a coding session: **build** against the frozen contract.
//!
//! A bounded attempt loop. An attempt ends one of three ways and they are not the same thing: it
//! RAN (a verdict), it got STUCK (ask a human — being stuck is the strongest reason to interrupt
//! someone), or the environment BROKE (fail fast; no answer you can type fixes a dead sandbox).
//! Conflating those is what made the ask seam unreachable from the case that most needed it.

use liberado_coder_core::{
    CoderRoleConfig, CoderRunRequest, CoderTask, CodingMode, GoalContract, SandboxSpec,
};
use liberado_common::Outcome;
use liberado_session::{
    GoalResult, GoalSpec, InputChannel, PackContext, PackError, SessionEvent, SessionEventKind,
    TerminalKind,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc::Sender;

use super::CodingSessionPack;
use super::policies::WorkspacePolicies;
use crate::CodingGoalPayload;

/// Is this a failure a **human answer** could plausibly unblock?
///
/// `NoChanges` = the model could not make progress. `Validation` = it could not satisfy a gate.
/// Both are "I am stuck", which is exactly when a person is worth interrupting — and both used to
/// kill the session outright, because the ask seam only ever ran on the success path.
///
/// Everything else (`Setup`, `Sandbox`, `Provider`, `Tool`, `Backend`) is a broken *environment*.
/// No answer you could type fixes a dead sandbox or an unreachable provider, so those still fail
/// fast: paging a human for them would be noise, and the whole value of the ask is that it is rare.
///
/// Whether `dir` is inside a git working tree — **not** merely whether it is a repo *root*.
///
/// `dir.join(".git").exists()` only answers for a root. A workspace pointed at a subdirectory of an
/// existing checkout would look like "not a repo", and [`init_git_repo`] would then create a nested
/// repository plus a placeholder commit inside someone's project. `rev-parse` answers the question
/// actually being asked.
fn is_git_repo(dir: &std::path::Path) -> bool {
    liberado_common::process::std_command("git")
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
    let ok = liberado_common::process::std_command("git")
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
    let _ = liberado_common::process::std_command("git")
        .args(["-C", &dir.to_string_lossy()])
        .args(["add", ".liberado-placeholder"])
        .status();
    let _ = liberado_common::process::std_command("git")
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
        payload: &CodingGoalPayload,
        payload_json: &serde_json::Value,
        ctx: &PackContext<'_>,
        contract: Option<Box<GoalContract>>,
        events: Sender<SessionEvent>,
        mut inputs: InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        let may_ask = ctx.can(&liberado_common::Capability::AskHuman);

        // ── Phase 2: build against the frozen contract ──────────────────────────────────────
        let workspace = payload
            .workspace_root()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                self.default_workspace_parent
                    .join(format!("goal-{session_id}"))
            });

        let _ = std::fs::create_dir_all(&workspace);
        init_git_repo(&workspace);

        // Payload wins, then the configured `[coder.coder]` role. The old fallback was the
        // literal `"session-coder"`, which is not a model any provider knows — and it did not
        // matter, because `SingleProviderFactory` ignored the requested model entirely. With a
        // model-aware factory installed this string is what actually gets called.
        let model = payload
            .model()
            .unwrap_or(self.coder_role.model.as_str())
            .to_string();

        // ── Parallel coding subagents (S6) ──────────────────────────────────────────────────
        // payload.subtasks → worktrees → LLM merge. Parent-only merge; children never self-merge.
        // Prefer hub-spawned child goal sessions when the pack has an attached hub; fall back to
        // in-process backend workers (tests). Nested fan-out is refused. Returns the terminal
        // GoalResult when subtasks ran; otherwise the single-agent build continues below.
        if let Some(result) = self
            .maybe_run_fanout(
                session_id,
                goal,
                payload,
                payload_json,
                ctx,
                &workspace,
                &model,
                &events,
            )
            .await?
        {
            return Ok(result);
        }

        // Path/command policy from profile overrides + payload
        // (plan = restricted write preset; explore = read-only preset).
        let policies =
            WorkspacePolicies::resolve(ctx.overrides(), payload_json, self.hashline.clone());
        let prompt = policies.coder_prompt(
            payload_json,
            // Loaded from prompts/coder/session-pack-coder.md, not a literal: the daemon path's
            // prompt was a second copy of coder instructions that nobody could diff against the
            // other two, and retuning it cost a rebuild.
            &liberado_coder_core::prompts::load(
                Some(&liberado_coder_core::prompts::dir_for(
                    None,
                    &workspace.to_string_lossy(),
                )),
                liberado_coder_core::prompts::SESSION_PACK_CODER_FILE,
                liberado_coder_core::prompts::SESSION_PACK_CODER,
            ),
        );

        let max_turns = self.resolve_max_turns(&policies, goal);
        let role = CoderRoleConfig {
            model: model.clone(),
            prompt_path: None,
            prompt: Some(prompt),
            temperature: Some(0.1),
            max_tokens: None,
            max_turns: Some(max_turns),
            reasoning: None,
        };

        let mut task = CoderTask::new(session_id, &goal.description);
        task.success_criteria = goal.success_criteria.clone();

        // Explore is read-only: HostLocal is enough (no worktree isolation required for readers).
        // Fan-out children already sit on a dedicated worktree — force HostLocal to avoid nesting.
        // Build mode on a git repo: **durable** session worktree under coding-worktrees/{session_id}
        // + HostLocal (S4). Ephemeral Worktree Drop would delete the FS root that shadow-git
        // checkpoints and mid-build park/resume need to survive attempt teardown (C7 + E6-c(b)).
        let (attempt_workspace, sandbox) =
            select_attempt_workspace(session_id, payload, &policies, &workspace, &events).await?;

        // One production assembly path shared with ACP and the headless runner. Surface-only
        // inputs (mode policies, pack coder role, leave verifiers empty for the contract) stay
        // here; shared knobs come from `self.tuning` so they cannot silently diverge again.
        let mut tuning = self.tuning.clone();
        // Keep mirror fields (with_gate / with_progress / …) authoritative when tests set them
        // without going through with_tuning.
        tuning.gate = self.gate.clone();
        tuning.progress = self.progress.clone();
        tuning.trace_dir = self.trace_dir.clone();
        tuning.trace_formats = self.trace_formats.clone();
        tuning.hashline = self.hashline.clone();
        tuning.coder = self.coder_role.clone();

        let assembled = crate::assemble_production_run(
            &tuning,
            crate::assemble::entry::pack_surface(crate::assemble::entry::PackSurfaceArgs {
                task,
                workspace_path: attempt_workspace.clone(),
                sandbox,
                coder_role: role,
                mode: policies.mode,
                command_policy: policies.command_policy.clone(),
                path_policy: policies.path_policy.clone(),
                hashline: policies.hashline.clone(),
            }),
        );
        let mut request = assembled.request;

        // Announce a restricted tier so the human sees why the session cannot write or shell out.
        send_mode_notice(session_id, &policies, &events).await;

        // The frozen contract overwrites description, success criteria, and — the point of the
        // whole exercise — the **verifiers**. Without it these stay empty and the loop grades its
        // own homework; with it, the gates are the human's, stamped with a content hash the worker
        // cannot alter.
        apply_contract_if_any(&contract, &mut request, session_id, &events).await?;

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
            let outcome = self
                .run_one_attempt(session_id, &model, &request, &events, &mut cancel)
                .await?;
            let (ok, summary, artifacts, diagnostics) = match outcome {
                AttemptOutcome::Broken(result) => return Ok(result),
                AttemptOutcome::Verdict {
                    ok,
                    summary,
                    artifacts,
                    diagnostics,
                } => (ok, summary, artifacts, diagnostics),
            };

            // Succeeded, or failed with no ask left to spend: this is the outcome.
            if ok || asks_remaining == 0 {
                return Ok(terminal_outcome(
                    session_id,
                    goal,
                    &request,
                    &events,
                    ok,
                    summary,
                    artifacts,
                    diagnostics,
                )
                .await);
            }

            match self
                .ask_for_guidance(session_id, ctx, &events, &mut inputs, &mut cancel, &summary)
                .await?
            {
                // Nobody answered inside the idle budget. The work stands; say so plainly.
                HumanAnswer::NoAnswer => {
                    return Ok(GoalResult {
                        terminal: TerminalKind::BudgetExhausted,
                        summary: format!(
                            "build failed and no answer to mid-run question: {summary}"
                        ),
                        artifacts,
                        diagnostics,
                    });
                }
                // The human aborted — the run stops, the work stands.
                HumanAnswer::Aborted => {
                    return Ok(GoalResult {
                        terminal: TerminalKind::Cancelled,
                        summary: format!("build failed; human aborted after: {summary}"),
                        artifacts,
                        diagnostics,
                    });
                }
                HumanAnswer::Guidance(guidance) => {
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

    /// Fan-out branch: refuse nested subtasks, then run the parallel coding subagents (S6) to a
    /// terminal GoalResult. `Ok(None)` when the payload carries no subtasks — the caller proceeds
    /// with the single-agent build.
    #[allow(clippy::too_many_arguments)]
    async fn maybe_run_fanout(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        payload: &CodingGoalPayload,
        payload_json: &serde_json::Value,
        ctx: &PackContext<'_>,
        workspace: &Path,
        model: &str,
        events: &Sender<SessionEvent>,
    ) -> Result<Option<GoalResult>, PackError> {
        if payload.fanout_child() && crate::fanout::subtasks_from_payload(payload_json).is_some() {
            return Err(PackError::Setup(
                "fanout child sessions cannot nest further subtasks".into(),
            ));
        }
        let Some(subtasks) = crate::fanout::subtasks_from_payload(payload_json) else {
            return Ok(None);
        };
        let max_concurrent = payload_json
            .get("max_concurrent_subagents")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                ctx.overrides()
                    .get("max_concurrent_coding_subagents")
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(self.max_concurrent_coding_subagents as u64)
            .max(1) as usize;

        let via_hub = self.hub().is_some();
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::Progress {
                    message: format!(
                        "coding fan-out: {} subtask(s), max_concurrent={max_concurrent}, mode={}",
                        subtasks.len(),
                        if via_hub {
                            "hub-sessions"
                        } else {
                            "in-process"
                        }
                    ),
                },
            ))
            .await;
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "coder-fanout".into(),
                    model: model.to_string(),
                },
            ))
            .await;

        let report = if let Some(hub) = self.hub() {
            crate::fanout::run_coding_fanout_via_hub(
                hub,
                ctx.grant.clone(),
                session_id,
                Arc::clone(&self.provider),
                workspace,
                subtasks,
                max_concurrent,
                model,
            )
            .await
        } else {
            crate::fanout::run_coding_fanout(
                Arc::clone(&self.backend),
                Arc::clone(&self.provider),
                workspace,
                subtasks,
                max_concurrent,
                model,
            )
            .await
        }
        .map_err(|e| PackError::Failed(format!("coding fan-out: {e}")))?;

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleFinished {
                    role: "coder-fanout".into(),
                },
            ))
            .await;
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::ValidationFinished {
                    ok: report.overall == liberado_common::Outcome::Succeeded,
                    summary: report.summary.clone(),
                },
            ))
            .await;

        let files = report
            .children
            .iter()
            .flat_map(|c| c.files_changed.iter().cloned())
            .collect::<Vec<_>>();
        for path in &files {
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::FileChanged {
                        path: path.clone(),
                        change: "modified".into(),
                    },
                ))
                .await;
        }

        let mut terminal = if report.overall == liberado_common::Outcome::Succeeded {
            TerminalKind::Succeeded
        } else {
            TerminalKind::Failed
        };
        let mut summary = report.summary.clone();
        let mut diagnostics = serde_json::json!({ "fanout": report });
        if terminal == TerminalKind::Succeeded
            && super::preflight_hook::ship_preflight_required(goal)
            && let Some(spec) = super::preflight_hook::ship_spec_from_goal(goal)
        {
            match super::preflight_hook::run_ship_preflight(session_id, workspace, &spec, events)
                .await
            {
                Ok(pf) => {
                    diagnostics = serde_json::json!({
                        "fanout": report,
                        "preflight": pf,
                    });
                    if !pf.ok {
                        terminal = TerminalKind::Failed;
                        summary = pf.summary;
                    }
                }
                Err(e) => {
                    return Ok(Some(GoalResult {
                        terminal: TerminalKind::Failed,
                        summary: format!("ship preflight error: {e}"),
                        artifacts: files,
                        diagnostics: serde_json::json!({
                            "fanout": report,
                            "preflight_error": e,
                        }),
                    }));
                }
            }
        }
        Ok(Some(GoalResult {
            terminal,
            summary,
            artifacts: files,
            diagnostics,
        }))
    }

    /// The attempt-turn bound for this goal, honouring the mode presets: exploration is bounded
    /// research (10), plans are short (8), and a configured `[coder.coder].max_turns` ceiling
    /// that the pack overrode with a smaller constant is worse than no setting at all — the
    /// operator sees 30 in the file and the run stops at 12.
    fn resolve_max_turns(&self, policies: &WorkspacePolicies, goal: &GoalSpec) -> u32 {
        if policies.explore_mode() {
            // Exploration is bounded research, not a long build.
            if goal.max_turns > 0 {
                10.min(goal.max_turns)
            } else {
                10
            }
        } else if policies.plan_mode() {
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
            // `[coder.coder].max_turns` when set, not a bare 12. A configured ceiling that the
            // pack overrode with a smaller constant is worse than no setting at all: the operator
            // sees 30 in the file and the run stops at 12.
            self.coder_role.max_turns.unwrap_or(12)
        }
    }

    /// Race one coding attempt against cancel (best-effort; LiberadoLoopBackend is not
    /// cancel-aware), then classify how it ended.
    async fn run_one_attempt(
        &self,
        session_id: &str,
        model: &str,
        request: &CoderRunRequest,
        events: &Sender<SessionEvent>,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<AttemptOutcome, PackError> {
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "coder".into(),
                    model: model.to_string(),
                },
            ))
            .await;

        use crate::live::LIVE_GATE;
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

        match result {
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
                Ok(AttemptOutcome::Verdict {
                    ok,
                    summary: r.summary,
                    artifacts: r.files_changed,
                    diagnostics: r.diagnostics,
                })
            }
            Err(e) if crate::is_stuck_error(&e) => {
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
                Ok(AttemptOutcome::Verdict {
                    ok: false,
                    summary: msg,
                    artifacts: Vec::new(),
                    diagnostics: serde_json::json!({"error": "coder_backend", "stuck": true}),
                })
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
                Ok(AttemptOutcome::Broken(GoalResult {
                    terminal: TerminalKind::Failed,
                    summary: msg,
                    artifacts: vec![],
                    diagnostics: serde_json::json!({"error": "coder_backend"}),
                }))
            }
        }
    }

    /// Ask the human how to proceed after a failed attempt. `Guidance` folds into the next
    /// attempt; the terminal answers carry no attempt state, so the caller builds their
    /// GoalResult from the attempt's artifacts and diagnostics.
    async fn ask_for_guidance(
        &self,
        session_id: &str,
        ctx: &PackContext<'_>,
        events: &Sender<SessionEvent>,
        inputs: &mut InputChannel,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
        summary: &str,
    ) -> Result<HumanAnswer, PackError> {
        let prompt = format!(
            "The build did not succeed:\n{summary}\n\nHow should I proceed?              Reply with guidance, or \"abort\" to stop."
        );
        let answer = self
            .ask(
                session_id,
                ctx,
                events,
                inputs,
                cancel,
                prompt,
                vec!["abort".into(), "retry".into()],
            )
            .await?;

        Ok(match answer {
            None => HumanAnswer::NoAnswer,
            Some(text)
                if text.trim().eq_ignore_ascii_case("abort")
                    || text.trim().eq_ignore_ascii_case("stop")
                    || text.trim().eq_ignore_ascii_case("cancel") =>
            {
                HumanAnswer::Aborted
            }
            Some(guidance) => HumanAnswer::Guidance(guidance),
        })
    }
}

/// How one coding attempt ended.
enum AttemptOutcome {
    /// The attempt ran and produced a verdict (pass/fail, or a stuck error) — continue the loop.
    Verdict {
        ok: bool,
        summary: String,
        artifacts: Vec<String>,
        diagnostics: serde_json::Value,
    },
    /// The environment broke (`Setup`/`Sandbox`/`Provider`/`Tool`/`Backend`) — the goal fails
    /// now; no human answer fixes a dead sandbox.
    Broken(GoalResult),
}

/// What the human said to a mid-run question.
enum HumanAnswer {
    /// Nobody answered inside the idle budget.
    NoAnswer,
    /// The human asked to stop.
    Aborted,
    /// Guidance to fold into the next attempt.
    Guidance(String),
}

/// Select the attempt workspace and sandbox: HostLocal for explore/fan-out children, a durable
/// session worktree under `coding-worktrees/{session_id}` for build mode on a git repo (S4).
async fn select_attempt_workspace(
    session_id: &str,
    payload: &CodingGoalPayload,
    policies: &WorkspacePolicies,
    workspace: &Path,
    events: &Sender<SessionEvent>,
) -> Result<(PathBuf, SandboxSpec), PackError> {
    let force_host = payload.force_host_local() || payload.fanout_child();
    if policies.explore_mode() || force_host {
        return Ok((workspace.to_path_buf(), SandboxSpec::HostLocal));
    }
    if is_git_repo(workspace) {
        let base = liberado_coder_tools::coding_worktrees_base();
        match liberado_coder_sandbox::ensure_session_worktree(workspace, session_id, &base).await {
            Ok(sess) => {
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::Progress {
                            message: format!(
                                "session workspace: {} (durable; survives park)",
                                sess.display()
                            ),
                        },
                    ))
                    .await;
                Ok((sess, SandboxSpec::HostLocal))
            }
            Err(e) => Err(PackError::Setup(format!("durable session worktree: {e}"))),
        }
    } else {
        Ok((workspace.to_path_buf(), SandboxSpec::HostLocal))
    }
}

/// Announce a restricted tier so the human sees why the session cannot write or shell out.
async fn send_mode_notice(
    session_id: &str,
    policies: &WorkspacePolicies,
    events: &Sender<SessionEvent>,
) {
    let mode_notice = match policies.mode {
        CodingMode::Normal => None,
        CodingMode::Plan => Some(format!(
            "plan mode: writes limited to {}; shell disabled",
            liberado_coder_core::PLAN_ARTIFACT_REL
        )),
        CodingMode::Explore => {
            Some("explore mode: read-only tools; writes and shell disabled".to_string())
        }
    };
    if let Some(message) = mode_notice {
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::Progress { message },
            ))
            .await;
    }
}

/// Verify and apply the frozen contract: the gates are only meaningful if they are the ones the
/// human accepted, so a contract that no longer matches its own hash is refused rather than built
/// against.
async fn apply_contract_if_any(
    contract: &Option<Box<GoalContract>>,
    request: &mut CoderRunRequest,
    session_id: &str,
    events: &Sender<SessionEvent>,
) -> Result<(), PackError> {
    if let Some(contract) = contract {
        // Prove the gates are the ones the human accepted before handing them to the worker it
        // will grade — a contract that no longer matches its own hash is a broken promise.
        contract
            .verify_integrity()
            .map_err(|e| PackError::Setup(format!("refusing to build: {e}")))?;
        contract.apply_to_request(request);
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
    Ok(())
}

/// Terminal outcome for a succeeded goal (or a failed one with no ask left to spend): run the
/// CI-equivalent ship preflight when the terminal state would be Succeeded.
#[allow(clippy::too_many_arguments)]
async fn terminal_outcome(
    session_id: &str,
    goal: &GoalSpec,
    request: &CoderRunRequest,
    events: &Sender<SessionEvent>,
    ok: bool,
    summary: String,
    artifacts: Vec<String>,
    diagnostics: serde_json::Value,
) -> GoalResult {
    let mut terminal = if ok {
        TerminalKind::Succeeded
    } else {
        TerminalKind::Failed
    };
    let mut summary = summary;
    let mut diagnostics = diagnostics;
    // Ship preflight: CI-equivalent project bar before terminal Succeeded.
    if terminal == TerminalKind::Succeeded
        && super::preflight_hook::ship_preflight_required(goal)
        && let Some(spec) = super::preflight_hook::ship_spec_from_goal(goal)
    {
        let preflight_root = request.workspace.root.as_str();
        let root_path = PathBuf::from(preflight_root);
        match super::preflight_hook::run_ship_preflight(session_id, &root_path, &spec, events).await
        {
            Ok(report) => {
                diagnostics = serde_json::json!({
                    "build": diagnostics,
                    "preflight": report,
                });
                if !report.ok {
                    terminal = TerminalKind::Failed;
                    summary = report.summary;
                }
            }
            Err(e) => {
                return GoalResult {
                    terminal: TerminalKind::Failed,
                    summary: format!("ship preflight error: {e}"),
                    artifacts,
                    diagnostics: serde_json::json!({
                        "build": diagnostics,
                        "preflight_error": e,
                    }),
                };
            }
        }
    }
    GoalResult {
        terminal,
        summary,
        artifacts,
        diagnostics,
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

#[cfg(test)]
#[path = "build_survivor_tests.rs"]
mod survivor_tests;
