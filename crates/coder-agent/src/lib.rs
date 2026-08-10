//! Coding **domain pack** for Liberado's agentic orchestration.
//!
//! This crate composes the shared inner loop (`liberado-executor`) with coding tools, sandbox,
//! deterministic verifiers, progress guards, optional critic, and attempt/repair. It is a domain
//! specialization — not the center of Liberado. See
//! `docs/architecture/agentic-loops.md` and `docs/roadmap/agentic-mesh-hygiene-audit-2026-07-10.md`.

mod completion_gate;
mod critic;
mod fanout;
mod gates;
mod intake_session;
mod live;
mod planner;
mod progress;
mod repair_feedback;
mod roles;
mod runtime;
mod session_pack;
mod trace;
mod verify_pipeline;

pub use fanout::{
    ChildOutcome, CodingSubtask, DEFAULT_MAX_CONCURRENT_CODING_SUBAGENTS, FanoutReport, MergeStep,
    child_session_grant, run_coding_fanout, run_coding_fanout_via_hub, subtasks_from_payload,
};
pub use intake_session::{
    IntakeAnswer, freeze_if_ready, request_from_contract, run_intake, run_intake_until_ready,
};
/// Shadow-git checkpoint store (S4).
pub use liberado_coder_sandbox::{Checkpoint, CheckpointError, ShadowGit};
/// Durable coding session workspace path (`coding-worktrees/<session_id>`).
pub use liberado_coder_tools::durable_session_workspace;
pub use live::with_live_events;
pub use session_pack::CodingSessionPack;

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use liberado_coder_core::{
    CoderBackend, CoderError, CoderEvent, CoderRoleConfig, CoderRunRequest, CoderRunResult,
    CriticVerdict, LIBERADO_LOOP_BACKEND, resolve_verifier_specs,
};
use liberado_coder_tools::CodingToolRuntime;
use liberado_common::Outcome;
use liberado_executor::{Budget, Executor, Task};
use liberado_provider::Provider;
use progress::ProgressGuard;
use serde_json::json;

/// Selects a [`Provider`] per role name (coder, repair, critic, …).
pub trait CoderProviderFactory: Send + Sync {
    fn provider_for(
        &self,
        role: &str,
        config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError>;
}

#[derive(Clone)]
pub struct SingleProviderFactory {
    provider: Arc<dyn Provider>,
}

impl SingleProviderFactory {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}

impl CoderProviderFactory for SingleProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        _config: &CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, CoderError> {
        Ok(self.provider.clone())
    }
}

/// Liberado's home-spun coding goal-session backend (`CoderBackend` implementation).
#[derive(Clone)]
pub struct LiberadoLoopBackend {
    providers: Arc<dyn CoderProviderFactory>,
}

impl LiberadoLoopBackend {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self::with_provider_factory(Arc::new(SingleProviderFactory::new(provider)))
    }

    pub fn with_provider_factory(providers: Arc<dyn CoderProviderFactory>) -> Self {
        Self { providers }
    }
}

#[async_trait]
impl CoderBackend for LiberadoLoopBackend {
    fn name(&self) -> &str {
        LIBERADO_LOOP_BACKEND
    }

    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let max_attempts = request.config.progress.max_attempts.max(1);
        let mut feedback = request.prior_feedback.clone();
        let mut last_retryable: Option<CoderError> = None;

        // Completion-gate strategist state. `refutations` is the *consecutive* count the gate's
        // threshold is defined against; `directive` carries a proposed structural change into the
        // next attempt (and every attempt after it, until a fresh one replaces it — a structural
        // change stays true while it is being worked on).
        //
        // A retryable error (NoChanges / Validation) neither increments nor resets the count.
        // Incrementing would let environmental flakiness summon a strategist that has no
        // refutations to reason about; resetting would let a run alternating error/refute/error
        // never reach the threshold at all, which is precisely the non-convergence the strategist
        // exists to break.
        let mut consecutive_refutations: u32 = 0;
        let mut strategist_directive = request.strategist_directive.clone();

        for attempt_offset in 0..max_attempts {
            let mut attempt_request = request.clone();
            attempt_request.attempt = request.attempt.saturating_add(attempt_offset);
            attempt_request.prior_feedback = feedback.clone();
            attempt_request.strategist_directive = strategist_directive.clone();

            match self.run_attempt(attempt_request).await {
                Ok(result) => {
                    let revision_issues = if let Some(CriticVerdict::NeedsRevision { issues }) =
                        &result.critic_verdict
                    {
                        Some(issues.clone())
                    } else {
                        None
                    };
                    match revision_issues {
                        Some(issues) if attempt_offset + 1 < max_attempts => {
                            let err = CoderError::Backend(format!(
                                "critic requested revision: {}",
                                issues.join("; ")
                            ));
                            feedback.push(repair_feedback::format_error_feedback(&err));
                            last_retryable = Some(err);

                            // Non-convergence check. Consult the strategist only once the same
                            // kind of refusal has repeated `strategist_after` times — a run that
                            // is still absorbing feedback does not need its approach rethought,
                            // and asking too early spends a model call to be told what the
                            // reviewers already said.
                            consecutive_refutations += 1;
                            let gate = liberado_session::CompletionGate {
                                fresh_reviewers: request.config.gate.fresh_reviewers,
                                quorum: liberado_session::Quorum::StrictMajorityOfFresh,
                                strategist_after: request.config.gate.strategist_after,
                            };
                            if request.config.gate.enabled
                                && gate.should_consult_strategist(consecutive_refutations)
                            {
                                // `attempt_request` was moved into `run_attempt`; rebuild just
                                // what the strategist reads, and only on the rare path where it
                                // actually runs rather than cloning on every attempt.
                                let mut strategist_request = request.clone();
                                strategist_request.attempt =
                                    request.attempt.saturating_add(attempt_offset);

                                // Best-effort: `run_strategist` swallows its own failures and
                                // returns None, so a strategist outage costs a directive, never
                                // the run.
                                if let Ok(Some(directive)) = completion_gate::run_strategist(
                                    self.providers.as_ref(),
                                    &strategist_request,
                                    &feedback,
                                )
                                .await
                                {
                                    strategist_directive = Some(directive);
                                    // The directive answers the refutations counted so far, so the
                                    // threshold restarts. Without this the strategist would fire on
                                    // every subsequent attempt, re-proposing against a history it
                                    // has already addressed.
                                    consecutive_refutations = 0;
                                }
                            }
                            continue;
                        }
                        Some(issues) => {
                            let mut failed = result;
                            failed.outcome = Outcome::Failed;
                            if !failed.summary.contains("critic") {
                                failed.summary = format!(
                                    "{}; critic requested revision: {}",
                                    failed.summary,
                                    issues.join("; ")
                                );
                            }
                            return Ok(failed);
                        }
                        None => return Ok(result),
                    }
                }
                Err(err) if is_retryable(&err) && attempt_offset + 1 < max_attempts => {
                    feedback.push(repair_feedback::format_error_feedback(&err));
                    last_retryable = Some(err);
                    continue;
                }
                Err(err) => return Err(err),
            }
        }

        Err(last_retryable.unwrap_or_else(|| {
            CoderError::Backend("coding attempts exhausted without a result".to_string())
        }))
    }
}

fn is_retryable(err: &CoderError) -> bool {
    matches!(err, CoderError::NoChanges | CoderError::Validation(_))
}

/// A single source of truth for retryable/stuck errors. `session_pack::build` calls this
/// so the same match arms don't need to be kept in sync across modules.
pub(crate) use is_retryable as is_stuck_error;

/// Shared git workspace diff for the critic and completion gate.
///
/// Assembles tracked diff against HEAD plus untracked file names, falling back to
/// `"(empty diff)"` when the workspace is clean. Used by both the legacy single-critic
/// path and the quorum-based completion gate.
pub(crate) async fn workspace_diff(workspace_root: &str) -> Result<String, CoderError> {
    let tracked = liberado_common::process::command("git")
        .args(["diff", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git diff: {e}")))?;
    if !tracked.status.success() {
        return Err(CoderError::Backend(format!(
            "git diff exited {:?}: {}",
            tracked.status.code(),
            String::from_utf8_lossy(&tracked.stderr)
        )));
    }
    let mut diff = String::from_utf8_lossy(&tracked.stdout).into_owned();

    let untracked = liberado_common::process::command("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(workspace_root)
        .output()
        .await
        .map_err(|e| CoderError::Backend(format!("git ls-files: {e}")))?;
    if untracked.status.success() {
        let names = String::from_utf8_lossy(&untracked.stdout);
        if !names.trim().is_empty() {
            if !diff.is_empty() {
                diff.push('\n');
            }
            diff.push_str("# untracked files\n");
            diff.push_str(&names);
        }
    }
    if diff.trim().is_empty() {
        diff = "(empty diff)".to_string();
    }
    Ok(diff)
}

impl LiberadoLoopBackend {
    async fn run_attempt(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let session_id = trace::session_id(&request);
        let events = Arc::new(Mutex::new(vec![CoderEvent::SessionStarted {
            session_id: session_id.clone(),
            backend: self.name().to_string(),
            task_id: request.task.id.clone(),
            at: Utc::now(),
        }]));

        // Optional planner (attempt 0 only) — inject plan into task context for the worker.
        let mut request = request;
        if request.attempt == 0
            && let Some(plan) =
                planner::run_planner(self.providers.as_ref(), &request, &events).await?
        {
            let block = plan.as_context_block();
            request.task.context = Some(match request.task.context.take() {
                Some(existing) => format!("{existing}\n\n{block}"),
                None => block,
            });
        }

        let worker_role_name = roles::worker_role_name(&request);
        let worker_config = roles::worker_role_config(&request);
        let max_turns = worker_config.max_turns.ok_or_else(|| {
            CoderError::Setup(format!(
                "{worker_role_name} role requires max_turns in resolved config"
            ))
        })?;
        let event_preview_max_chars = request.config.progress.event_preview_max_chars;

        let workspace_root_in = request.workspace.root.clone();
        // Pass the task/session id so Worktree isolation gets a unique directory name (not the
        // project folder name — self-host on `life-os` would otherwise collide and fail on Windows
        // extended paths under `…/worktrees/life-os`).
        let mut coding_runtime = CodingToolRuntime::from_sandbox_with_session(
            &workspace_root_in,
            request.config.sandbox.clone(),
            request.config.command_policy.clone(),
            request.config.path_policy.clone(),
            Some(request.task.id.as_str()),
        )
        .await
        .map_err(|e| CoderError::Tool(e.to_string()))?
        .with_hashline(request.config.hashline.clone());

        // The sandbox may have created a separate workspace (e.g. Worktree).
        // Use the actual workspace root for change detection, verification,
        // and gating so they operate on the worktree rather than the parent.
        let effective_root = coding_runtime
            .workspace_root()
            .to_string_lossy()
            .to_string();
        // S4: shadow-git checkpoints keyed by stable goal/task id (not per-attempt trace id).
        let checkpoint_key = if request.task.id.is_empty() {
            session_id.clone()
        } else {
            request.task.id.clone()
        };
        take_workspace_checkpoint(
            Path::new(&effective_root),
            &checkpoint_key,
            &format!("attempt-{}-start", request.attempt),
        )
        .await;
        // Capture HEAD *before* the worker runs so a clean tree after `git_commit` still counts
        // as real progress (dogfood finding #3 — porcelain is empty once the agent commits).
        let baseline_sha = gates::rev_parse(&effective_root, "HEAD").await.ok();
        if let Some(command) = &request.config.validation_command {
            coding_runtime =
                coding_runtime.with_validation_command(gates::command_request(command));
        }
        let progress = Arc::new(Mutex::new(ProgressGuard::new(
            request.config.progress.clone(),
        )));
        let runtime = runtime::GuardedTracingRuntime::new(
            coding_runtime,
            events.clone(),
            progress.clone(),
            event_preview_max_chars,
        );

        trace::push_event(
            &events,
            CoderEvent::RoleStarted {
                role: worker_role_name.to_string(),
                model: worker_config.model.clone(),
                at: Utc::now(),
            },
        );
        let provider = self
            .providers
            .provider_for(worker_role_name, worker_config)?;
        let mut instructions = roles::role_instructions(worker_config, worker_role_name).await?;
        if request.config.hashline.enabled {
            instructions.push_str(&liberado_coder_tools::hashline_prompt_guidance(
                request.config.hashline.hash_length,
            ));
        }
        let task = Task::new(instructions, roles::coder_goal(&request));
        let executor = Executor::new(provider, Budget::new(max_turns)).with_observer(Arc::new(
            trace::TurnTracer::new(events.clone(), worker_role_name),
        ));
        let report = executor
            .execute(&runtime, task)
            .await
            .map_err(|e| CoderError::Provider(e.to_string()))?;
        // Post-worker checkpoint captures mid-attempt FS state for park/resume (S4).
        take_workspace_checkpoint(
            Path::new(&effective_root),
            &checkpoint_key,
            &format!("attempt-{}-post", request.attempt),
        )
        .await;
        trace::push_event(
            &events,
            CoderEvent::RoleFinished {
                role: worker_role_name.to_string(),
                at: Utc::now(),
            },
        );
        trace::push_event(
            &events,
            CoderEvent::ReportFiled {
                outcome: report.outcome,
                summary: report.summary.clone(),
                at: Utc::now(),
            },
        );

        let fatal = progress
            .lock()
            .expect("progress mutex poisoned")
            .take_fatal();
        if let Some(fatal) = fatal {
            return Err(
                gates::fail_with_progress_fatal(&request, &session_id, &events, fatal).await,
            );
        }

        let file_changes: Vec<liberado_coder_core::FileChangeRecord> =
            gates::resolve_attempt_changes(&effective_root, baseline_sha.as_deref())
                .await?
                .into_iter()
                .map(|(path, change)| liberado_coder_core::FileChangeRecord {
                    path,
                    change: change.to_string(),
                })
                .collect();
        let files_changed: Vec<String> = file_changes.iter().map(|c| c.path.clone()).collect();
        if files_changed.is_empty() && report.outcome != Outcome::Failed {
            trace::push_event(
                &events,
                CoderEvent::LoopGuardTriggered {
                    guard: "no_changes".to_string(),
                    action: "fail_run".to_string(),
                    at: Utc::now(),
                },
            );
            trace::push_event(
                &events,
                CoderEvent::SessionFinished {
                    outcome: Outcome::Failed,
                    at: Utc::now(),
                },
            );
            let _ =
                trace::write_trace(&request, &session_id, trace::snapshot_events(&events), None)
                    .await;
            return Err(CoderError::NoChanges);
        }
        for path in &files_changed {
            trace::push_event(
                &events,
                CoderEvent::FileChanged {
                    path: path.clone(),
                    at: Utc::now(),
                },
            );
        }

        // Authoritative verifier pipeline (config list and/or legacy validation_command).
        // Skipped when the worker already reported Failed (honest stop).
        let mut validation_notes = None;
        let mut outcome = report.outcome;
        let mut summary = report.summary;
        // Verifier results outlive the pipeline block: the completion gate shows them to reviewers
        // as the deterministic floor they may not override.
        let mut verifier_results: Vec<liberado_coder_core::NamedVerdict> = Vec::new();
        if outcome != Outcome::Failed {
            let specs = resolve_verifier_specs(
                &request.config.verifiers,
                request.config.validation_command.as_ref(),
            );
            if !specs.is_empty() {
                let pipeline = verify_pipeline::run_pipeline(
                    &effective_root,
                    &specs,
                    &request.config.verify_policy,
                    Some(&events),
                )
                .await?;
                if !pipeline.is_pass() {
                    // Signature-aware feedback for repair routing (scratchpad C).
                    let feedback = repair_feedback::format_pipeline_repair(&pipeline);
                    trace::push_event(
                        &events,
                        CoderEvent::LoopGuardTriggered {
                            guard: "verifier_pipeline".to_string(),
                            action: "fail_run".to_string(),
                            at: Utc::now(),
                        },
                    );
                    trace::push_event(
                        &events,
                        CoderEvent::SessionFinished {
                            outcome: Outcome::Failed,
                            at: Utc::now(),
                        },
                    );
                    let _ = trace::write_trace(
                        &request,
                        &session_id,
                        trace::snapshot_events(&events),
                        None,
                    )
                    .await;
                    return Err(CoderError::Validation(feedback));
                }
                validation_notes = Some(
                    pipeline
                        .results
                        .iter()
                        .map(|r| format!("{}: {}", r.id, r.verdict.summary))
                        .collect::<Vec<_>>()
                        .join("; "),
                );
                verifier_results = pipeline.results.clone();
            }
        }

        // The judgment layer, on top of the deterministic verifiers above. Two shapes:
        //
        // * gate enabled  — a remembered gatekeeper plus a quorum of cold reviewers, adjudicated
        //   by the kernel (`liberado_session::CompletionGate`). Fail-closed.
        // * gate disabled — the legacy single critic, unchanged.
        //
        // Both are skipped when the worker already failed or changed nothing: there is no claim to
        // dispute, and asking a reviewer to bless an empty diff only burns tokens.
        let mut critic_verdict = None;
        let mut gate_votes = Vec::new();
        let reviewable = outcome != Outcome::Failed && !files_changed.is_empty();
        if reviewable && request.config.gate.enabled {
            let gate_outcome = completion_gate::run_gate(
                self.providers.as_ref(),
                &request,
                &verifier_results,
                &events,
            )
            .await?;
            gate_votes = completion_gate::flatten_votes(&gate_outcome);
            let verdict = match &gate_outcome.verdict {
                liberado_session::GateVerdict::Approved => CriticVerdict::Acceptable,
                liberado_session::GateVerdict::Refuted { issues } => {
                    // Belt and braces: `run`'s attempt loop also derives Failed from a
                    // `NeedsRevision` verdict, so this assignment is not the only thing standing
                    // between a refutation and a Succeeded result. It is kept so `run_attempt`'s
                    // own return value is self-consistent — a caller reading it directly (evals,
                    // future single-attempt callers) must never see Succeeded next to a refuted
                    // verdict. `critic_verdict`, not `outcome`, is the signal that drives retry.
                    outcome = Outcome::Failed;
                    summary = format!(
                        "{summary}; completion gate refused ({} of {} votes refuting): {}",
                        gate_outcome
                            .votes
                            .iter()
                            .filter(|v| !v.vote.is_approve())
                            .count(),
                        gate_outcome.votes.len(),
                        issues.join("; ")
                    );
                    CriticVerdict::NeedsRevision {
                        issues: issues.clone(),
                    }
                }
            };
            critic_verdict = Some(verdict);
        } else if reviewable && roles::critic_enabled(&request) {
            let verdict: CriticVerdict =
                critic::run_critic(self.providers.as_ref(), &request, &events).await?;
            trace::push_event(
                &events,
                CoderEvent::CriticVerdict {
                    verdict: verdict.clone(),
                    at: Utc::now(),
                },
            );
            if let CriticVerdict::NeedsRevision { issues } = &verdict {
                outcome = Outcome::Failed;
                summary = format!(
                    "{summary}; critic requested revision: {}",
                    issues.join("; ")
                );
            }
            critic_verdict = Some(verdict);
        }

        trace::push_event(
            &events,
            CoderEvent::SessionFinished {
                outcome,
                at: Utc::now(),
            },
        );

        let mut result = CoderRunResult {
            backend: self.name().to_string(),
            outcome,
            summary,
            files_changed,
            file_changes,
            validation_notes,
            critic_verdict,
            gate_votes,
            trace_path: None,
            diagnostics: json!({
                "artifacts_reported": report.artifacts,
                "attempt": request.attempt,
                "worker_role": worker_role_name,
            }),
        };
        result.trace_path = trace::write_trace(
            &request,
            &session_id,
            trace::snapshot_events(&events),
            Some(result.clone()),
        )
        .await?;
        Ok(result)
    }
}

/// Best-effort shadow-git snapshot of `workspace_root`, keyed by `session_key`.
/// Emits a live `Checkpoint` event when the coding pack's LIVE_GATE is installed.
async fn take_workspace_checkpoint(workspace_root: &Path, session_key: &str, label: &str) {
    let Ok(sg) = liberado_coder_sandbox::ShadowGit::open_or_init(workspace_root, session_key)
    else {
        return;
    };
    match sg.snapshot(label).await {
        Ok(cp) => {
            live::emit(liberado_session::SessionEventKind::Checkpoint {
                id: cp.id.clone(),
                label: cp.label.clone(),
                tree_hash: cp.tree_hash.clone(),
            });
            tracing::debug!(
                session = %session_key,
                checkpoint = %cp.id,
                label = %cp.label,
                "coding checkpoint taken"
            );
        }
        Err(e) => {
            tracing::warn!(
                session = %session_key,
                error = %e,
                "coding checkpoint snapshot failed (non-fatal)"
            );
        }
    }
}

/// Serializes the tests that set `LIBERADO_DATA_DIR`.
///
/// The variable is process-global but several tests across this crate point it at their own
/// tempdir and then remove it — `fanout`'s three merge tests and `session_pack`'s worktree test.
/// Run concurrently in one test binary (which is how `cargo test` runs a crate's unit tests), one
/// test's `remove_var` lands while another is mid-run, `coding_worktrees_base()` silently falls
/// back to `.liberado`, and the fan-out merge fails against a directory it never wrote to. That
/// showed up as an intermittent `fanout_two_children_clean_merge` failure that always passed when
/// re-run alone.
///
/// Every test that touches the variable must hold this guard for as long as it depends on the
/// value.
///
/// A `tokio` mutex rather than a `std` one because the guard is held across the awaits that make
/// up the test body; a blocking guard held across an await can stall the runtime, which is what
/// `clippy::await_holding_lock` warns about. This one yields instead.
#[cfg(test)]
pub(crate) static DATA_DIR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_coder_core::{
        CoderRoleConfig, CoderRunConfig, CoderTask, CoderTrace, CommandPolicy, PathPolicy,
        ProgressPolicy, SandboxSpec, WorkspaceRef,
    };
    use liberado_provider::{CompletionResponse, MockProvider, ToolInvocation};
    use serde_json::json;

    fn role() -> CoderRoleConfig {
        CoderRoleConfig {
            model: "mock".to_string(),
            prompt_path: None,
            prompt: Some("Edit the repo and report when done.".to_string()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(6),
        }
    }

    fn disabled_role() -> CoderRoleConfig {
        CoderRoleConfig {
            model: "mock".to_string(),
            prompt_path: None,
            prompt: None,
            temperature: None,
            max_tokens: None,
            max_turns: Some(4),
        }
    }

    fn request(root: &std::path::Path, base_ref: &str) -> CoderRunRequest {
        CoderRunRequest {
            task: CoderTask::new("task-1", "write hello.txt"),
            workspace: WorkspaceRef::new(root.to_string_lossy(), base_ref),
            config: CoderRunConfig {
                backend: LIBERADO_LOOP_BACKEND.to_string(),
                trace_dir: None,
                trace_formats: Vec::new(),
                planner: disabled_role(),
                coder: role(),
                critic: disabled_role(),
                gate: liberado_coder_core::CoderGateConfig::default(),
                repair: None,
                sandbox: SandboxSpec::HostLocal,
                command_policy: CommandPolicy::default(),
                validation_command: None,
                verifiers: Vec::new(),
                verify_policy: Default::default(),
                path_policy: PathPolicy::default(),
                progress: ProgressPolicy::default(),
                hashline: liberado_coder_core::HashlineConfig::default(),
            },
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        }
    }

    fn request_with_role(
        root: &std::path::Path,
        base_ref: &str,
        coder: CoderRoleConfig,
    ) -> CoderRunRequest {
        let mut request = request(root, base_ref);
        request.config.coder = coder;
        request
    }

    struct RecordingProviderFactory {
        provider: Arc<dyn Provider>,
        calls: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl CoderProviderFactory for RecordingProviderFactory {
        fn provider_for(
            &self,
            role: &str,
            config: &CoderRoleConfig,
        ) -> Result<Arc<dyn Provider>, CoderError> {
            self.calls
                .lock()
                .unwrap()
                .push((role.to_string(), config.model.clone()));
            Ok(self.provider.clone())
        }
    }

    fn write_then_report() -> [CompletionResponse; 2] {
        [
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "write-1",
                "write_file",
                json!({"path": "hello.txt", "content": "hello\n"}),
            )]),
            CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "Wrote hello.txt",
                    "artifacts": ["hello.txt"],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )]),
        ]
    }

    #[tokio::test]
    async fn mocked_loop_edits_workspace_and_reports_changed_file() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let result = backend.run(request(dir.path(), "HEAD")).await.unwrap();

        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.files_changed, vec!["hello.txt"]);
    }

    /// Mocked end-to-end: hashline enabled → tool catalog offers `hashline_edit`, system
    /// prompt includes guidance, and a precomputed-tag patch mutates the workspace.
    #[tokio::test]
    async fn mocked_loop_hashline_edit_and_prompt_wiring() {
        use liberado_coder_core::HashlineConfig;

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let seed = "alpha\nbeta\ngamma\n";
        std::fs::write(dir.path().join("notes.txt"), seed).unwrap();
        run(dir.path(), &["git", "add", "."]);
        run(dir.path(), &["git", "commit", "-m", "seed notes"]);

        let tag = liberado_coder_tools::hashline_compute_file_hash(seed, 6);
        let patch = format!("[notes.txt#{tag}]\nPUT 2.=2:\n+BETA\n");

        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "hl-1",
                    "hashline_edit",
                    json!({ "input": patch }),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Patched notes.txt via hashline",
                        "artifacts": ["notes.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let backend = LiberadoLoopBackend::new(provider.clone());
        let mut request = request(dir.path(), "HEAD");
        request.task.description = "Change beta to BETA in notes.txt using hashline.".into();
        request.config.hashline = HashlineConfig {
            enabled: true,
            hash_length: 6,
        };
        request.config.coder.prompt =
            Some("You are a coding agent. Use tools then submit_report.".into());

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert!(
            result.files_changed.iter().any(|p| p == "notes.txt"),
            "files_changed={:?}",
            result.files_changed
        );
        let after = std::fs::read_to_string(dir.path().join("notes.txt")).unwrap();
        assert_eq!(after, "alpha\nBETA\ngamma\n");

        // First completion request must advertise hashline_edit and carry prompt guidance.
        let first = provider
            .received_requests()
            .into_iter()
            .next()
            .expect("provider received a completion request");
        let tool_names: Vec<&str> = first.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            tool_names.contains(&"hashline_edit"),
            "tools={tool_names:?}"
        );
        let system = first
            .messages
            .iter()
            .find(|m| m.role == liberado_provider::Role::System)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        assert!(
            system.contains("Hashline edit mode") || system.contains("hashline_edit"),
            "system prompt missing hashline guidance:\n{system}"
        );
        assert!(
            system.contains('6') || system.contains("6-char"),
            "system prompt should mention configured hash length"
        );
    }

    #[tokio::test]
    async fn mocked_loop_hashline_disabled_omits_tool_from_catalog() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider.clone());
        let mut request = request(dir.path(), "HEAD");
        request.config.hashline = liberado_coder_core::HashlineConfig {
            enabled: false,
            hash_length: 4,
        };
        backend.run(request).await.unwrap();
        let first = provider.received_requests().into_iter().next().unwrap();
        let tool_names: Vec<&str> = first.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            !tool_names.contains(&"hashline_edit"),
            "hashline_edit must be absent when disabled; tools={tool_names:?}"
        );
    }

    #[tokio::test]
    async fn backend_asks_provider_factory_for_coder_role_model() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
                provider,
                calls: calls.clone(),
            }));
        let mut request = request(dir.path(), "HEAD");
        request.config.coder.model = "deepseek/deepseek-v4-pro".to_string();

        backend.run(request).await.unwrap();

        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &[("coder".to_string(), "deepseek/deepseek-v4-pro".to_string())]
        );
    }

    #[tokio::test]
    async fn internal_git_status_is_not_blocked_by_model_command_policy() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.command_policy.deny = vec!["git status".to_string()];

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.files_changed, vec!["hello.txt"]);
    }

    #[tokio::test]
    async fn writes_trace_when_trace_dir_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.trace_dir = Some(dir.path().join("traces").to_string_lossy().to_string());

        let result = backend.run(request).await.unwrap();
        let trace_path = result.trace_path.as_ref().expect("trace path");
        let trace_json = std::fs::read_to_string(trace_path).unwrap();
        let trace: CoderTrace = serde_json::from_str(&trace_json).unwrap();

        assert_eq!(trace.result.unwrap().summary, "Wrote hello.txt");
        assert!(trace.events.iter().any(|event| {
            matches!(
                event,
                CoderEvent::FileChanged { path, .. } if path == "hello.txt"
            )
        }));
    }

    #[tokio::test]
    async fn trace_keeps_full_tool_args_regardless_of_the_live_stream_cap() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "abcdefghijklmnopqrstuvwxyz"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Wrote hello.txt",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.trace_dir = Some(dir.path().join("traces").to_string_lossy().to_string());
        request.config.progress.event_preview_max_chars = 12;

        let result = backend.run(request).await.unwrap();
        let trace_json = std::fs::read_to_string(result.trace_path.unwrap()).unwrap();
        let trace: CoderTrace = serde_json::from_str(&trace_json).unwrap();
        let args_preview = trace
            .events
            .iter()
            .find_map(|event| match event {
                CoderEvent::ToolStarted { args_preview, .. } => Some(args_preview),
                _ => None,
            })
            .expect("tool args preview");

        // `event_preview_max_chars` is 12 here. It sizes the excerpt shown on the live session
        // stream, and used to size the trace as well — which meant the diagnostic record of a run
        // was clipped to whatever felt readable in a chat pane. The model is handed the tool's full
        // arguments and full output, so a trace clipped below that cannot explain what it did.
        assert!(
            args_preview.contains("abcdefghijklmnopqrstuvwxyz"),
            "the trace must keep the whole argument the tool was actually called with, not the \
             first {} characters of it: {args_preview}",
            12
        );
        assert!(
            args_preview.chars().count() <= trace::TRACE_MAX_CHARS,
            "still bounded — by the trace's own ceiling, not the live stream's"
        );
    }

    #[tokio::test]
    async fn coder_role_requires_resolved_max_turns() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::new("mock"));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.coder.max_turns = None;

        let err = backend.run(request).await.unwrap_err();

        assert!(matches!(err, CoderError::Setup(_)));
        assert!(err.to_string().contains("max_turns"));
    }

    #[tokio::test]
    async fn configured_validation_gate_sets_notes_on_success() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.validation_command = Some(test_command("validation-ok"));

        let result = backend.run(request).await.unwrap();

        assert_eq!(result.outcome, Outcome::Succeeded);
        // Pass note summarizes check ids (legacy command becomes id "validate").
        assert!(result.validation_notes.unwrap().contains("validate"));
    }

    #[tokio::test]
    async fn verifier_paths_exist_fails_incomplete_success() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        // Write hello.txt but pipeline requires missing_required.txt
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.progress.max_attempts = 1;
        request.config.verifiers = vec![liberado_coder_core::VerifierSpec::PathsExist {
            id: "must".into(),
            paths: vec!["missing_required.txt".into()],
        }];

        let err = backend.run(request).await.unwrap_err();
        assert!(matches!(err, CoderError::Validation(_)));
        assert!(err.to_string().contains("missing_required"));
    }

    #[tokio::test]
    async fn configured_validation_gate_fails_run_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.progress.max_attempts = 1;
        request.config.validation_command = Some(failing_test_command());

        let err = backend.run(request).await.unwrap_err();

        assert!(matches!(err, CoderError::Validation(_)));
        // Pipeline feedback names the check id (legacy command → "validate") and failure.
        let msg = err.to_string();
        assert!(
            msg.contains("validate") || msg.contains("exited") || msg.contains("Completeness"),
            "unexpected validation message: {msg}"
        );
    }

    #[tokio::test]
    async fn success_report_without_diff_is_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "succeeded",
                    "summary": "Done",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )])],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.progress.max_attempts = 1;
        let err = backend.run(request).await.unwrap_err();
        assert!(matches!(err, CoderError::NoChanges));
    }

    #[tokio::test]
    async fn loads_coder_prompt_from_prompt_path() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let prompt_path = dir.path().join("coder.md");
        std::fs::write(&prompt_path, "Prompt loaded from disk.").unwrap();
        let mut coder = role();
        coder.prompt = None;
        coder.prompt_path = Some(prompt_path.to_string_lossy().to_string());

        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::tool_calls(vec![ToolInvocation::new(
                "report-1",
                liberado_executor::SUBMIT_REPORT_TOOL,
                json!({
                    "outcome": "failed",
                    "summary": "No edit requested",
                    "artifacts": [],
                    "new_high_signal_facts": [],
                    "follow_up": null
                }),
            )])],
        ));
        let backend = LiberadoLoopBackend::new(provider.clone());
        let result = backend
            .run(request_with_role(dir.path(), "HEAD", coder))
            .await
            .unwrap();

        assert_eq!(result.outcome, Outcome::Failed);
        let sent = provider.last_request().unwrap();
        assert!(
            sent.messages[0]
                .content
                .contains("Prompt loaded from disk.")
        );
    }

    #[tokio::test]
    async fn read_only_stall_fails_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "l1",
                    "list_files",
                    json!({}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "l2",
                    "list_files",
                    json!({}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "l3",
                    "list_files",
                    json!({}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "l4",
                    "list_files",
                    json!({}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "explored only",
                        "artifacts": [],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.coder.max_turns = Some(10);
        request.config.progress.read_only_turn_limit = 2;
        request.config.progress.same_tool_limit = 100;
        request.config.progress.max_attempts = 1;

        let err = backend.run(request).await.unwrap_err();
        assert!(matches!(err, CoderError::NoChanges));
    }

    #[tokio::test]
    async fn critic_accepts_diff() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "hello\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Wrote hello.txt",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
                CompletionResponse::text(r#"{"quality":"acceptable"}"#),
            ],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.critic = CoderRoleConfig {
            model: "mock-critic".to_string(),
            prompt_path: None,
            prompt: Some("Review the diff strictly.".to_string()),
            temperature: Some(0.0),
            max_tokens: Some(512),
            max_turns: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.critic_verdict, Some(CriticVerdict::Acceptable));
    }

    #[tokio::test]
    async fn critic_needs_revision_fails_final_attempt() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "hello\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Wrote hello.txt",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
                CompletionResponse::text(
                    r#"{"quality":"needs_revision","issues":["missing tests"]}"#,
                ),
            ],
        ));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.progress.max_attempts = 1;
        request.config.critic = CoderRoleConfig {
            model: "mock-critic".to_string(),
            prompt_path: None,
            prompt: Some("Review the diff strictly.".to_string()),
            temperature: None,
            max_tokens: None,
            max_turns: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        assert!(matches!(
            result.critic_verdict,
            Some(CriticVerdict::NeedsRevision { issues }) if issues.iter().any(|i| i.contains("tests"))
        ));
    }

    #[tokio::test]
    async fn planner_runs_before_coder_when_configured() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::text(
                    r#"{"summary":"write hello","steps":["create hello.txt"],"likely_files":["hello.txt"],"risks":[]}"#,
                ),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "hello\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Wrote hello.txt",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
                provider: provider.clone(),
                calls: calls.clone(),
            }));
        let mut request = request(dir.path(), "HEAD");
        request.config.planner = CoderRoleConfig {
            model: "mock-planner".to_string(),
            prompt_path: None,
            prompt: Some("Plan the task briefly.".to_string()),
            temperature: Some(0.0),
            max_tokens: Some(512),
            max_turns: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Succeeded);
        let roles: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|(role, _)| role.clone())
            .collect();
        assert_eq!(roles, vec!["planner".to_string(), "coder".to_string()]);
        // Worker goal should include planner plan (second request is the worker complete).
        let requests = provider.received_requests();
        assert!(requests.len() >= 2);
        let worker_user = requests[1]
            .messages
            .iter()
            .find(|m| m.role == liberado_provider::Role::User)
            .map(|m| m.content.as_str())
            .unwrap_or("");
        assert!(
            worker_user.contains("Planner plan") || worker_user.contains("hello.txt"),
            "worker should see plan context: {worker_user}"
        );
    }

    #[tokio::test]
    async fn validation_failure_uses_signature_feedback_for_repair() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        // Attempt 0: write notes.txt only (missing required path) → validation fail
        // Attempt 1 (repair): write required.txt + report
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "notes.txt", "content": "incomplete\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "claimed done",
                        "artifacts": ["notes.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-2",
                    "write_file",
                    json!({"path": "required.txt", "content": "ok\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-2",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "fixed gates",
                        "artifacts": ["required.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
                provider: provider.clone(),
                calls: calls.clone(),
            }));
        let mut request = request(dir.path(), "HEAD");
        request.config.progress.max_attempts = 2;
        request.config.verifiers = vec![liberado_coder_core::VerifierSpec::PathsExist {
            id: "must".into(),
            paths: vec!["required.txt".into()],
        }];
        request.config.repair = Some(CoderRoleConfig {
            model: "mock-repair".to_string(),
            prompt_path: None,
            prompt: Some("Repair: satisfy frozen verifiers.".to_string()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(6),
        });

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert!(result.files_changed.iter().any(|p| p.contains("required")));
        let roles: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|(role, _)| role.clone())
            .collect();
        assert_eq!(roles, vec!["coder".to_string(), "repair".to_string()]);
        // Repair goal should include FAILURE_CLASS routing.
        let requests = provider.received_requests();
        let repair_msgs = requests.last().map(|r| &r.messages).unwrap();
        let repair_blob = repair_msgs
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            repair_blob.contains("FAILURE_CLASS")
                || repair_blob.contains("missing_path")
                || repair_blob.contains("Repair focus"),
            "repair should see signature routing: {repair_blob}"
        );
    }

    #[tokio::test]
    async fn retries_no_changes_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            [
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-1",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Done",
                        "artifacts": [],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "write-1",
                    "write_file",
                    json!({"path": "hello.txt", "content": "hello\n"}),
                )]),
                CompletionResponse::tool_calls(vec![ToolInvocation::new(
                    "report-2",
                    liberado_executor::SUBMIT_REPORT_TOOL,
                    json!({
                        "outcome": "succeeded",
                        "summary": "Wrote hello.txt on retry",
                        "artifacts": ["hello.txt"],
                        "new_high_signal_facts": [],
                        "follow_up": null
                    }),
                )]),
            ],
        ));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let backend =
            LiberadoLoopBackend::with_provider_factory(Arc::new(RecordingProviderFactory {
                provider,
                calls: calls.clone(),
            }));
        let mut request = request(dir.path(), "HEAD");
        request.config.progress.max_attempts = 2;
        request.config.repair = Some(CoderRoleConfig {
            model: "mock-repair".to_string(),
            prompt_path: None,
            prompt: Some("Repair: actually write the file.".to_string()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(6),
        });

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.files_changed, vec!["hello.txt"]);
        let roles: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|(role, _)| role.clone())
            .collect();
        assert_eq!(roles, vec!["coder".to_string(), "repair".to_string()]);
    }

    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY and network access"]
    async fn openrouter_deepseek_live_coding_smoke() {
        use liberado_provider_openai_compat::OpenAiCompatibleProvider;

        let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY not set");
        let model = std::env::var("LIBERADO_CODER_LIVE_MODEL")
            .unwrap_or_else(|_| "deepseek/deepseek-v4-pro".to_string());
        let provider = Arc::new(
            OpenAiCompatibleProvider::new(
                api_key,
                &model,
                OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
            )
            .with_extra_client_error_status(vec![402]),
        );

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let mut request = request(dir.path(), "HEAD");
        request.task.description =
            "Create a file named hello.txt containing exactly: hello from liberado\n".to_string();
        request.config.coder.model = model;
        request.config.coder.prompt = Some(
            "You are a careful autonomous coding agent. Inspect the workspace when useful, make the requested code or file edits with the available tools, then submit a concise success report."
                .to_string(),
        );
        request.config.coder.max_turns = Some(10);
        request.config.progress.event_preview_max_chars = 1_000;
        request.config.progress.max_attempts = 1;

        let backend = LiberadoLoopBackend::new(provider);
        let result = backend.run(request).await.unwrap();

        assert_eq!(result.outcome, Outcome::Succeeded);
        assert!(result.files_changed.iter().any(|path| path == "hello.txt"));
        let content = std::fs::read_to_string(dir.path().join("hello.txt")).unwrap();
        // Models sometimes omit the trailing newline; smoke cares about the payload.
        assert_eq!(
            content.trim_end_matches(['\r', '\n']),
            "hello from liberado"
        );
    }

    /// Live smoke for hashline edit mode: an *existing* multi-line file must be patched
    /// via line anchors (not a greenfield `write_file` of hello.txt). Catches prompt/tool
    /// wiring bugs that unit tests miss.
    #[tokio::test]
    #[ignore = "requires OPENROUTER_API_KEY and network access"]
    async fn openrouter_deepseek_live_hashline_edit_smoke() {
        use liberado_coder_core::{HashlineConfig, VerifierSpec};
        use liberado_provider_openai_compat::OpenAiCompatibleProvider;

        let api_key = std::env::var("OPENROUTER_API_KEY").expect("OPENROUTER_API_KEY not set");
        let model = std::env::var("LIBERADO_CODER_LIVE_MODEL")
            .unwrap_or_else(|_| "deepseek/deepseek-v4-pro".to_string());
        let provider = Arc::new(
            OpenAiCompatibleProvider::new(
                api_key,
                &model,
                OpenAiCompatibleProvider::OPENROUTER_BASE_URL,
            )
            .with_extra_client_error_status(vec![402]),
        );

        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        // Multi-line file so a middle-line edit is meaningful.
        let seed = "\
# greet helper
def greet(name):
    msg = \"Hello, \" + name
    print(msg)
    return msg

if __name__ == \"__main__\":
    greet(\"world\")
";
        std::fs::write(dir.path().join("greet.py"), seed).unwrap();
        run(dir.path(), &["git", "add", "."]);
        run(dir.path(), &["git", "commit", "-m", "seed greet.py"]);

        let mut request = request(dir.path(), "HEAD");
        request.task.id = "hashline-live-1".into();
        request.task.description = "\
In greet.py, change ONLY the message construction so it uses an f-string: \
msg = f\"Hi, {name}\" instead of string concatenation. Do not rewrite the whole file \
if you can avoid it. Keep the rest of the file (function structure, print, return, \
__main__) intact.\n"
            .to_string();
        request.task.success_criteria = vec![
            "greet.py uses f\"Hi, {name}\" (or equivalent f-string Hi greeting)".into(),
            "greet.py still defines def greet(name)".into(),
        ];
        request.config.coder.model = model.clone();
        request.config.coder.prompt = Some(
            "You are a careful autonomous coding agent. Hashline edit mode is ENABLED.\n\
             - read_file returns [path#TAG] and LINE:content anchors.\n\
             - Prefer hashline_edit for existing files: pass a patch with [path#TAG] and \
             PUT/CUT ops using + body rows. Re-read after every edit because the tag changes.\n\
             - write_file is only for brand-new files. edit_file/apply_patch are fallbacks.\n\
             - When done, submit_report with outcome=succeeded only if the file really changed."
                .to_string(),
        );
        request.config.coder.max_turns = Some(16);
        request.config.hashline = HashlineConfig {
            enabled: true,
            hash_length: 6,
        };
        request.config.progress.event_preview_max_chars = 2_000;
        request.config.progress.max_attempts = 2;
        request.config.progress.read_only_turn_limit = 6;
        request.config.trace_dir = Some(dir.path().join("traces").to_string_lossy().into_owned());
        request.config.verifiers = vec![VerifierSpec::ContentContains {
            id: "hi-fstring".into(),
            path: "greet.py".into(),
            must_include: vec!["Hi,".into()],
        }];

        let backend = LiberadoLoopBackend::new(provider);
        let result = match backend.run(request).await {
            Ok(r) => r,
            Err(e) => panic!("hashline live smoke backend error: {e:#}"),
        };

        eprintln!(
            "hashline live smoke: outcome={:?} summary={} files={:?} diagnostics={}",
            result.outcome, result.summary, result.files_changed, result.diagnostics
        );
        if let Some(path) = &result.trace_path {
            eprintln!("trace: {path}");
            if let Ok(raw) = std::fs::read_to_string(path) {
                // Surface whether the model actually used hashline tools.
                let used_hashline = raw.contains("hashline_edit");
                let used_read = raw.contains("\"read_file\"") || raw.contains("read_file");
                eprintln!("trace tool hints: hashline_edit={used_hashline} read_file={used_read}");
                // Print tool names from events for diagnosis.
                for line in raw.lines().take(80) {
                    if line.contains("ToolStarted")
                        || line.contains("tool_started")
                        || line.contains("\"name\"")
                    {
                        eprintln!("trace-line: {line}");
                    }
                }
            }
        }

        let content = std::fs::read_to_string(dir.path().join("greet.py")).unwrap();
        eprintln!("--- greet.py after run ---\n{content}\n--- end ---");

        assert_eq!(
            result.outcome,
            Outcome::Succeeded,
            "expected success; summary={} validation={:?}",
            result.summary,
            result.validation_notes
        );
        assert!(
            result
                .files_changed
                .iter()
                .any(|p| p == "greet.py" || p.ends_with("greet.py")),
            "greet.py should be in files_changed: {:?}",
            result.files_changed
        );
        assert!(
            content.contains("def greet(name)"),
            "function signature must remain"
        );
        assert!(
            content.contains("Hi,") && content.contains("name"),
            "expected Hi greeting with name; got:\n{content}"
        );
        assert!(
            !content.contains("Hello, \" + name") && !content.contains("Hello, \"+ name"),
            "old concatenation should be gone; got:\n{content}"
        );
    }

    #[test]
    fn parses_git_status_paths() {
        assert_eq!(
            gates::parse_status_path("?? hello.txt"),
            Some("hello.txt".to_string())
        );
        assert_eq!(
            gates::parse_status_path("R  old.txt -> new.txt"),
            Some("new.txt".to_string())
        );
        assert_eq!(gates::parse_status_path(""), None);
    }

    #[test]
    fn parses_critic_json_with_fences() {
        let raw = "```json\n{\"quality\":\"acceptable\"}\n```";
        assert_eq!(
            critic::parse_critic_verdict(raw).unwrap(),
            CriticVerdict::Acceptable
        );
    }

    fn init_repo(root: &std::path::Path) {
        run(root, &["git", "init"]);
        run(root, &["git", "config", "user.email", "test@example.com"]);
        run(root, &["git", "config", "user.name", "Test User"]);
        std::fs::write(root.join("README.md"), "# test\n").unwrap();
        run(root, &["git", "add", "."]);
        run(root, &["git", "commit", "-m", "base"]);
    }

    fn run(root: &std::path::Path, command: &[&str]) {
        let status = std::process::Command::new(command[0])
            .args(&command[1..])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success(), "command failed: {command:?}");
    }

    fn test_command(message: &str) -> liberado_coder_core::CoderCommandConfig {
        #[cfg(windows)]
        {
            liberado_coder_core::CoderCommandConfig {
                program: "cmd".to_string(),
                args: vec!["/C".to_string(), format!("echo {message}")],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
            }
        }
        #[cfg(not(windows))]
        {
            liberado_coder_core::CoderCommandConfig {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), format!("echo {message}")],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
            }
        }
    }

    fn failing_test_command() -> liberado_coder_core::CoderCommandConfig {
        #[cfg(windows)]
        {
            liberado_coder_core::CoderCommandConfig {
                program: "cmd".to_string(),
                args: vec![
                    "/C".to_string(),
                    "echo validation-failed >&2 && exit /B 1".to_string(),
                ],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
            }
        }
        #[cfg(not(windows))]
        {
            liberado_coder_core::CoderCommandConfig {
                program: "sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "echo validation-failed >&2; exit 1".to_string(),
                ],
                env: Default::default(),
                timeout_secs: None,
                output_max_bytes: None,
            }
        }
    }
}
