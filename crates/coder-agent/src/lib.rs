//! Coding **domain pack** for Liberado's agentic orchestration.
//!
//! This crate composes the shared inner loop (`liberado-executor`) with coding tools, sandbox,
//! deterministic verifiers, progress guards, optional critic, and attempt/repair. It is a domain
//! specialization — not the center of Liberado. See
//! `docs/spec/architecture/agentic-loops.md` and `docs/future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md`.

pub mod assemble;
mod backend;
mod coding_goal;
pub mod cold_review;
mod completion_gate;
mod critic;
pub mod extension;
mod fanout;
mod finish_gate;
mod gates;
mod intake_session;
mod live;
mod planner;
mod progress;
pub mod remediation;
mod repair_feedback;
mod roles;
mod runtime;
pub mod session_critic;
mod session_pack;
mod trace;
mod verify_pipeline;

pub use assemble::{
    AssembledRun, AssemblyProvenance, CriticPolicy, EmptyVerifiersPolicy, FieldSource,
    ProductionSurface, RepairPolicy, TraceDirPolicy, assemble_production_run,
};
pub use backend::LiberadoLoopBackend;

pub use coding_goal::CodingGoalPayload;
pub use cold_review::{
    ChangeSurface, ColdFinding, ColdReviewRequest, DropReason, FilterResult,
    ForbiddenAuthorContext, MAX_FIX_ROUNDS, ReadyInputs, Severity, StageDecision,
    build_cold_review_request, cold_pr_reviewer_prompt, decide_after_filter,
    decide_after_fix_round, filter_findings, fix_round_task, ready_for_human,
};
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
pub use roles::COLD_DIFF_REVIEWER_PROMPT;
pub use session_pack::CodingSessionPack;

/// The ship bar: the CI-equivalent gate a coding run must clear before it may report success.
///
/// Public because more than one entry point dispatches coding runs. The daemon path reaches this
/// through [`CodingSessionPack`]; the ACP bridge builds its own request and, until the gate was
/// exported, had no way to reach the same decision — so every run dispatched from an ACP client
/// skipped the bar entirely and nothing said so. Re-exported rather than duplicated: two copies
/// of a merge bar drift, and the one that drifts is the one nobody is watching.
pub mod ship_preflight {
    pub use crate::session_pack::preflight_hook::{
        run_ship_preflight, ship_preflight_required, ship_preflight_required_for, ship_spec_for,
        ship_spec_from_goal,
    };
}

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use liberado_coder_core::{
    CoderError, CoderEvent, CoderRoleConfig, CoderRunRequest, CoderRunResult, CriticVerdict,
    resolve_verifier_specs,
};
use liberado_coder_tools::CodingToolRuntime;
use liberado_common::Outcome;
use liberado_executor::{Budget, Executor, MvlSession, Task};
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

impl LiberadoLoopBackend {
    /// Everything the run does before any post-run review: plan, work, verify, gate, repair.
    async fn run_attempts(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
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
        // Every diff-critic issue raised across every attempt, with the attempt that raised it.
        // What became of each is derived at the end by `derive_dispositions`; the loop only
        // records, because "was this fixed" is not answerable until the run stops.
        let mut raised: Vec<(u32, String)> = Vec::new();

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
                            raised.extend(issues.iter().map(|i| (attempt_offset, i.clone())));

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
                            raised.extend(issues.iter().map(|i| (attempt_offset, i.clone())));
                            failed.diff_findings = derive_dispositions(&raised, &issues);
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
                        None => {
                            // The final attempt was approved, so every issue ever raised was
                            // answered. Recording them as fixed is not bookkeeping: it is what
                            // lets the report say "four raised, four resolved" instead of
                            // silently discarding a reviewer's work.
                            let mut result = result;
                            result.diff_findings = derive_dispositions(&raised, &[]);
                            return Ok(result);
                        }
                    }
                }
                Err(err) if is_retryable(&err) && attempt_offset + 1 < max_attempts => {
                    let latest = repair_feedback::format_error_feedback(&err);
                    repair_feedback::prune_resolved_verifier_feedback(&mut feedback, &latest);
                    feedback.push(latest);
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

    /// Read the run's narration and attach any findings to the result.
    ///
    /// Deliberately swallows its own errors. This is a post-hoc review of finished work: a
    /// reviewer that failed to answer must not turn a completed run into a failed one, and it
    /// must not report a clean review either — a failed call leaves `session_findings` empty and
    /// logs, which reads as "not reviewed", not as "reviewed and fine".
    async fn review_session_after_run(
        &self,
        config: &liberado_coder_core::CoderRunConfig,
        result: &mut CoderRunResult,
    ) {
        if !config.session_critic.enabled {
            return;
        }
        let Some(trace) = result.trace_path.clone() else {
            tracing::warn!("session critic is enabled but the run wrote no trace; skipping");
            return;
        };
        let Some(events) = Self::read_trace_events(&trace).await else {
            return;
        };
        let summary = result.summary.clone();
        self.run_session_review(config, &events, &summary, result)
            .await;
    }

    /// Execute the session review against the loaded trace and record any findings on the result.
    async fn run_session_review(
        &self,
        config: &liberado_coder_core::CoderRunConfig,
        events: &[liberado_coder_core::CoderEvent],
        summary: &str,
        result: &mut CoderRunResult,
    ) {
        let role = config
            .session_critic
            .role
            .clone()
            .unwrap_or_else(|| config.critic.clone());
        let visibility = if config.session_critic.include_tool_names {
            session_critic::ToolVisibility::NamesOnly
        } else {
            session_critic::ToolVisibility::TextOnly
        };
        // The reviewer needs a request to name the task; the trace's own copy is the right one.
        let request = liberado_coder_core::CoderRunRequest {
            task: liberado_coder_core::CoderTask::new("session-review", summary.to_string()),
            workspace: liberado_coder_core::WorkspaceRef::new("", "HEAD"),
            config: config.clone(),
            attempt: 0,
            prior_feedback: Vec::new(),
            strategist_directive: None,
        };
        match session_critic::review_session(
            self.providers.as_ref(),
            &request,
            &role,
            events,
            Some(summary),
            visibility,
        )
        .await
        {
            Ok(review) => {
                if !review.is_clean() {
                    tracing::info!(
                        count = review.findings.len(),
                        "session critic raised findings"
                    );
                }
                result.session_findings = review.findings;
            }
            Err(e) => tracing::warn!(error = %e, "session critic failed; run left unreviewed"),
        }
    }

    /// Load a run's event trace. Any read/parse failure is logged and yields `None`, leaving the
    /// run unreviewed rather than failing it.
    async fn read_trace_events(trace: &str) -> Option<Vec<liberado_coder_core::CoderEvent>> {
        let raw = match tokio::fs::read_to_string(trace).await {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(error = %e, path = %trace, "session critic: cannot read trace");
                return None;
            }
        };
        match serde_json::from_str::<liberado_coder_core::CoderTrace>(&raw) {
            Ok(t) => Some(t.events),
            Err(e) => {
                tracing::warn!(error = %e, "session critic: unreadable trace");
                None
            }
        }
    }
}

/// Work out what happened to each diff-critic issue over the life of a run.
///
/// An issue raised on one attempt and absent from the final verdict was answered; an issue in the
/// final verdict is still standing. Deduplicated by text, keeping the earliest attempt that
/// raised it, because the same complaint restated three times is one complaint.
///
/// String equality is the matching rule, and it is imperfect: a reviewer that rewords the same
/// objection produces a second entry. That errs toward showing a reader *more* than happened,
/// which is the right direction for a mechanism whose whole purpose is that findings do not get
/// buried.
pub(crate) fn derive_dispositions(
    raised: &[(u32, String)],
    final_issues: &[String],
) -> Vec<liberado_coder_core::DiffFinding> {
    use liberado_coder_core::{DiffFinding, Disposition};
    let mut out: Vec<DiffFinding> = Vec::new();
    for (attempt, issue) in raised {
        if out.iter().any(|f| &f.issue == issue) {
            continue;
        }
        out.push(DiffFinding {
            issue: issue.clone(),
            disposition: if final_issues.contains(issue) {
                Disposition::Outstanding
            } else {
                Disposition::Fixed
            },
            first_seen_attempt: *attempt,
        });
    }
    out
}

fn is_retryable(err: &CoderError) -> bool {
    match err {
        // NoChanges is a stall, not a new strategy. Retrying it identical
        // burned three 30-turn Flash attempts in the sequential compare.
        CoderError::NoChanges => false,
        // A validation failure normally means the change is wrong, and another attempt is the
        // right answer. It does not mean that when the machine is what failed: a full disk
        // reproduces on every attempt, so retrying spends the budget to reach the same place.
        // The class is carried in the message by `format_pipeline_repair`.
        CoderError::Validation(msg) => !msg.contains(&format!(
            "FAILURE_CLASS: {}",
            repair_feedback::FailureClass::Infrastructure.as_str()
        )),
        _ => false,
    }
}

/// Stuck enough to ask a human. Broader than [`is_retryable`]: a read-only
/// exhausted attempt is stuck, but another identical attempt will not help.
pub(crate) fn is_stuck_error(err: &CoderError) -> bool {
    matches!(err, CoderError::NoChanges) || is_retryable(err)
}

/// Whether the event log already says how the attempt ended.
///
/// Used to avoid stamping `SessionAborted` on top of a body that handled its own failure and said
/// so — a trace claiming both a decision and a crash describes neither.
fn ended_in_trace(events: &trace::EventLog) -> bool {
    trace::snapshot_events(events).iter().any(|e| {
        matches!(
            e,
            CoderEvent::SessionFinished { .. } | CoderEvent::SessionAborted { .. }
        )
    })
}

/// How much untracked file content the critic's diff may carry, across all new files.
///
/// Larger than the tool's budget: a reviewer reads the change once and has the whole context
/// window for it, where the tool's output competes with everything else in a working turn.
const UNTRACKED_REVIEW_BUDGET: usize = 60_000;

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
        let paths: Vec<&str> = names
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if !paths.is_empty() {
            if !diff.is_empty() {
                diff.push('\n');
            }
            diff.push_str("# untracked files (new, not yet added)\n");
            // Names alone were not enough. A reviewer handed "session_store.rs" and no content
            // can only report that it cannot see the file, which is what happened: the critic
            // raised the file's absence as a finding while 334 lines of it sat on disk. The main
            // artifact of a task is often a file that did not exist before, and a review of a
            // change that omits its largest part is not a review.
            let mut budget = UNTRACKED_REVIEW_BUDGET;
            for path in paths {
                diff.push_str(&format!("--- new file {path}\n"));
                let Ok(body) = tokio::fs::read(Path::new(workspace_root).join(path)).await else {
                    diff.push_str("(unreadable)\n");
                    continue;
                };
                if body.contains(&0) {
                    diff.push_str("(binary)\n");
                    continue;
                }
                let text = String::from_utf8_lossy(&body);
                let shown = liberado_coder_tools::truncate_on_char(&text, budget);
                budget = budget.saturating_sub(shown.len());
                diff.push_str(shown);
                if shown.len() < text.len() {
                    if !shown.ends_with('\n') {
                        diff.push('\n');
                    }
                    diff.push_str("… truncated\n");
                } else if !shown.ends_with('\n') {
                    diff.push('\n');
                }
            }
        }
    }
    if diff.trim().is_empty() {
        diff = "(empty diff)".to_string();
    }
    Ok(diff)
}

/// Assemble the executor for one worker attempt: the observer that traces each turn, the
/// workspace-compile finish gate (a `succeeded` report is not accepted while `cargo check` is
/// red), and the spill directory oversized tool results are offloaded into.
fn build_executor(
    provider: Arc<dyn Provider>,
    max_turns: u32,
    events: &trace::EventLog,
    worker_role_name: &str,
    effective_root: &str,
) -> Executor {
    Executor::new(provider, Budget::new(max_turns))
        .with_observer(Arc::new(trace::TurnTracer::new(
            events.clone(),
            worker_role_name,
        )))
        .with_report_gate(Arc::new(finish_gate::WorkspaceCompileGate::new(
            effective_root.to_string(),
        )))
        .with_spill_dir(Path::new(effective_root).join(".liberado").join("offload"))
}

/// Attach the MVL (model-verifier loop) session when `trace_dir` is configured. A failed open is
/// logged and the run continues without it — tracing must never fail a coding run.
fn attach_mvl_if_configured(
    executor: Executor,
    trace_dir: Option<&str>,
    session_id: &str,
) -> Executor {
    let Some(dir) = trace_dir else {
        return executor;
    };
    let mvl_path = Path::new(dir).join(format!("{session_id}.mvl.jsonl"));
    let exec_path = Path::new(dir).join(format!("{session_id}.execution.jsonl"));
    match MvlSession::open(&mvl_path, Some(&exec_path), session_id.to_string()) {
        Ok(session) => executor.with_mvl(Arc::new(session)),
        Err(error) => {
            tracing::warn!(
                session_id = %session_id,
                error = %error,
                "MVL session failed to open; coding run continues without it"
            );
            executor
        }
    }
}

/// The executor already filed a report; this guard turns a worker that changed nothing (and did
/// not itself file a failure) into a `NoChanges` error the caller does not retry. Compare 7 C1:
/// the model submitted `succeeded` after a compiling change set; `same_tool_churn` had latched on
/// twenty `run_command` searches, and this site turned that into `NoChanges` (not retried). The
/// ship bar still runs below. A latched inspect stall must not outrank a report the model was
/// told to file. Records both trace events the original inline block emitted.
fn no_changes_guard(events: &trace::EventLog) -> CoderError {
    trace::push_event(
        events,
        CoderEvent::LoopGuardTriggered {
            guard: "no_changes".to_string(),
            action: "fail_run".to_string(),
            at: Utc::now(),
        },
    );
    trace::push_event(
        events,
        CoderEvent::SessionFinished {
            outcome: Outcome::Failed,
            at: Utc::now(),
        },
    );
    CoderError::NoChanges
}

/// Workspace-side state prepared once per attempt, before the executor runs. Kept as a struct so
/// `attempt_body` reads as prepare → run → report, with the pieces the post-run stages consume
/// (root, baseline, progress) named once.
struct WorkerRuntime {
    effective_root: String,
    checkpoint_key: String,
    baseline_sha: Option<String>,
    progress: Arc<Mutex<ProgressGuard>>,
    runtime: runtime::GuardedTracingRuntime,
}

/// The executor already filed a report; a latched progress-fatal is logged and ignored. Compare
/// 7 C1: the model submitted `succeeded` after a compiling change set; `same_tool_churn` had
/// latched on twenty `run_command` searches, and this site turned that into `NoChanges` (not
/// retried). The ship bar still runs below. A latched inspect stall must not outrank a report the
/// model was told to file.
fn log_ignored_fatal(progress: &Arc<Mutex<ProgressGuard>>, outcome: Outcome) {
    if let Some(fatal) = progress
        .lock()
        .expect("progress mutex poisoned")
        .take_fatal()
    {
        tracing::warn!(
            guard = fatal.guard_name(),
            outcome = ?outcome,
            "progress fatal ignored; a report was filed"
        );
    }
}

/// Resolve the files this attempt changed against its baseline HEAD, as typed change records.
async fn resolve_attempt_changes(
    effective_root: &str,
    baseline_sha: Option<&str>,
) -> Result<Vec<liberado_coder_core::FileChangeRecord>, CoderError> {
    Ok(gates::resolve_attempt_changes(effective_root, baseline_sha)
        .await?
        .into_iter()
        .map(|(path, change)| liberado_coder_core::FileChangeRecord {
            path,
            change: change.to_string(),
        })
        .collect())
}

impl LiberadoLoopBackend {
    /// Run one attempt and **write its trace on every exit path**.
    ///
    /// The body below returns early through a dozen `?` operators. Each one used to discard the
    /// entire event log, which meant the attempt that failed in a way nobody had anticipated was
    /// precisely the attempt that left no trace — the inverse of what a debugger needs.
    ///
    /// It was measured: one run put 122 tool calls on the wire and 76 into trace files, and the
    /// missing 46 were the attempt that ended on `critic returned empty content`. That error
    /// travels out of [`critic::run_critic`] through a `?` that sits *before* the write.
    ///
    /// So the write lives here, wrapped around the body, where no future `?` can route around it.
    /// Adding one inside [`Self::attempt_body`] is now safe by construction rather than by care.
    async fn run_attempt(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
        let session_id = trace::session_id(&request);
        let events = Arc::new(Mutex::new(vec![CoderEvent::SessionStarted {
            session_id: session_id.clone(),
            backend: self.backend_name().to_string(),
            task_id: request.task.id.clone(),
            at: Utc::now(),
        }]));

        let outcome = self
            .attempt_body(request.clone(), &session_id, &events)
            .await;

        // An error that reached here was not handled by the body, so nothing has said the attempt
        // ended. Record what killed it: "the attempt failed" without the reason is the state that
        // made the last four failures cost a day each.
        if let Err(e) = &outcome
            && !ended_in_trace(&events)
        {
            trace::push_event(
                &events,
                CoderEvent::SessionAborted {
                    error: e.to_string(),
                    at: Utc::now(),
                },
            );
        }

        let written = trace::write_trace(
            &request,
            &session_id,
            trace::snapshot_events(&events),
            outcome.as_ref().ok().cloned(),
        )
        .await;

        match (outcome, written) {
            // A trace is a diagnostic. Failing a completed run because its diagnostic could not be
            // written repeats #119's mistake — the disk being full is not a verdict on the change.
            (Ok(mut result), Ok(path)) => {
                result.trace_path = path;
                Ok(result)
            }
            (Ok(result), Err(e)) => {
                tracing::warn!(session_id = %session_id, error = %e, "trace write failed; run stands");
                Ok(result)
            }
            (Err(original), _) => Err(original),
        }
    }

    /// The role resolved from the request: its name and the max-turns budget its config
    /// requires. A role without `max_turns` is a configuration error, refused up front.
    fn resolve_worker_role(request: &CoderRunRequest) -> Result<(String, u32), CoderError> {
        let worker_role_name = roles::worker_role_name(request);
        let worker_config = roles::worker_role_config(request);
        let max_turns = worker_config.max_turns.ok_or_else(|| {
            CoderError::Setup(format!(
                "{worker_role_name} role requires max_turns in resolved config"
            ))
        })?;
        Ok((worker_role_name.to_string(), max_turns))
    }

    /// Everything the worker run needs that lives on the workspace side: the runtime (with the
    /// sandbox's *effective* root — it may have created a separate worktree), the start
    /// checkpoint keyed by stable goal/task id (not per-attempt trace id, S4), the pre-run HEAD
    /// (so a clean tree after `git_commit` still counts as progress), and the progress/runtime
    /// wrappers the executor drives.
    async fn prepare_worker_runtime(
        request: &CoderRunRequest,
        session_id: &str,
        events: &trace::EventLog,
        event_preview_max_chars: usize,
        extensions: &[Arc<dyn extension::RuntimeExtension>],
    ) -> Result<WorkerRuntime, CoderError> {
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
        .with_hashline(request.config.hashline.clone())
        .with_offered_tools(request.config.offered_tools.clone());

        // The sandbox may have created a separate workspace (e.g. Worktree).
        // Use the actual workspace root for change detection, verification,
        // and gating so they operate on the worktree rather than the parent.
        let effective_root = coding_runtime
            .workspace_root()
            .to_string_lossy()
            .to_string();
        // S4: shadow-git checkpoints keyed by stable goal/task id (not per-attempt trace id).
        let checkpoint_key = if request.task.id.is_empty() {
            session_id.to_string()
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
        )
        .with_extensions(extensions);
        Ok(WorkerRuntime {
            effective_root,
            checkpoint_key,
            baseline_sha,
            progress,
            runtime,
        })
    }

    /// Resolve the role's provider and assemble the executor's task: role instructions plus the
    /// optional hashline prompt guidance, over the request's goal.
    async fn build_worker_task(
        &self,
        request: &CoderRunRequest,
        worker_role_name: &str,
        events: &trace::EventLog,
    ) -> Result<(Arc<dyn Provider>, Task), CoderError> {
        let worker_config = roles::worker_role_config(request);
        trace::push_event(
            events,
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
        let task = Task::new(instructions, roles::coder_goal(request));
        Ok((provider, task))
    }

    /// Optional planner (attempt 0 only): run the planner and inject its plan into the task
    /// context for the worker, preserving any context the caller already provided (the plan is
    /// appended after it).
    async fn apply_planner_plan(
        &self,
        request: &mut CoderRunRequest,
        events: &trace::EventLog,
    ) -> Result<(), CoderError> {
        if request.attempt == 0
            && let Some(plan) =
                planner::run_planner(self.providers.as_ref(), request, events).await?
        {
            let block = plan.as_context_block();
            request.task.context = Some(match request.task.context.take() {
                Some(existing) => format!("{existing}\n\n{block}"),
                None => block,
            });
        }
        Ok(())
    }

    async fn attempt_body(
        &self,
        request: CoderRunRequest,
        session_id: &str,
        events: &trace::EventLog,
    ) -> Result<CoderRunResult, CoderError> {
        let events = events.clone();
        let session_id = session_id.to_string();

        // Optional planner (attempt 0 only) — inject plan into task context for the worker.
        let mut request = request;
        self.apply_planner_plan(&mut request, &events).await?;

        let (worker_role_name, max_turns) = Self::resolve_worker_role(&request)?;
        let event_preview_max_chars = request.config.progress.event_preview_max_chars;
        let WorkerRuntime {
            effective_root,
            checkpoint_key,
            baseline_sha,
            progress,
            runtime,
        } = Self::prepare_worker_runtime(
            &request,
            &session_id,
            &events,
            event_preview_max_chars,
            &self.extensions,
        )
        .await?;

        let (provider, task) = self
            .build_worker_task(&request, &worker_role_name, &events)
            .await?;
        let mut executor = build_executor(
            provider,
            max_turns,
            &events,
            &worker_role_name,
            &effective_root,
        );
        executor =
            attach_mvl_if_configured(executor, request.config.trace_dir.as_deref(), &session_id);
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

        log_ignored_fatal(&progress, report.outcome);

        let file_changes =
            resolve_attempt_changes(&effective_root, baseline_sha.as_deref()).await?;
        let files_changed: Vec<String> = file_changes.iter().map(|c| c.path.clone()).collect();
        if files_changed.is_empty() && report.outcome != Outcome::Failed {
            return Err(no_changes_guard(&events));
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
        // Authoritative verifier pipeline (config list and/or legacy validation_command).
        // Skipped when the worker already reported Failed (honest stop).
        let VerifierOutcome {
            notes: validation_notes,
            results: verifier_results,
        } = run_verifier_pipeline(
            &request,
            &effective_root,
            baseline_sha.as_ref(),
            &events,
            report.outcome,
        )
        .await?;

        let mut state = VerdictState {
            outcome: report.outcome,
            summary: report.summary,
            critic_verdict: None,
            gate_votes: Vec::new(),
        };
        apply_judgment(
            self.providers.as_ref(),
            &request,
            &verifier_results,
            &files_changed,
            &events,
            &mut state,
        )
        .await?;

        trace::push_event(
            &events,
            CoderEvent::SessionFinished {
                outcome: state.outcome,
                at: Utc::now(),
            },
        );

        let result = CoderRunResult {
            backend: self.backend_name().to_string(),
            outcome: state.outcome,
            summary: state.summary,
            files_changed,
            file_changes,
            validation_notes,
            critic_verdict: state.critic_verdict,
            gate_votes: state.gate_votes,
            trace_path: None,
            diff_findings: Vec::new(),
            session_findings: Vec::new(),
            remediation: None,
            diagnostics: json!({
                "artifacts_reported": report.artifacts,
                "attempt": request.attempt,
                "worker_role": worker_role_name,
            }),
        };
        Ok(result)
    }
}

/// What the authoritative verifier pipeline produced for one attempt: the human-readable note
/// plus the per-verifier results the completion gate may show to reviewers.
struct VerifierOutcome {
    notes: Option<String>,
    results: Vec<liberado_coder_core::NamedVerdict>,
}

/// The authoritative verifier pipeline (config list and/or legacy `validation_command`).
/// Skipped when the worker already reported `Failed` (honest stop). Verifier results outlive the
/// pipeline block: the completion gate shows them to reviewers as the deterministic floor they
/// may not override.
async fn run_verifier_pipeline(
    request: &CoderRunRequest,
    effective_root: &str,
    baseline_sha: Option<&String>,
    events: &trace::EventLog,
    outcome: Outcome,
) -> Result<VerifierOutcome, CoderError> {
    if outcome == Outcome::Failed {
        return Ok(VerifierOutcome {
            notes: None,
            results: Vec::new(),
        });
    }
    let specs = resolve_verifier_specs(
        &request.config.verifiers,
        request.config.validation_command.as_ref(),
    );
    if specs.is_empty() {
        return Ok(VerifierOutcome {
            notes: None,
            results: Vec::new(),
        });
    }
    let mut pipeline = verify_pipeline::run_pipeline(
        effective_root,
        &specs,
        &request.config.verify_policy,
        Some(events),
    )
    .await?;
    // A test failure that already existed on the base commit is not the agent's fault. Compare
    // named cargo-test failures against `compute_baseline` (throwaway worktree at
    // HEAD-before-edits, cached per commit). If every failure is pre-existing, the test verifier
    // is treated as passing.
    if !pipeline.is_pass()
        && let Some(baseline_sha) = baseline_sha
    {
        let baseline_failures = gates::baseline_test_failures(effective_root, baseline_sha)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "baseline test comparison failed; treating all cargo-test failures as new"
                );
                std::collections::BTreeSet::new()
            });
        pipeline = soften_pre_existing_test_failures(&pipeline, &baseline_failures);
    }
    if !pipeline.is_pass() {
        // Signature-aware feedback for repair routing (scratchpad C).
        let feedback = repair_feedback::format_pipeline_repair(&pipeline);
        trace::push_event(
            events,
            CoderEvent::LoopGuardTriggered {
                guard: "verifier_pipeline".to_string(),
                action: "fail_run".to_string(),
                at: Utc::now(),
            },
        );
        trace::push_event(
            events,
            CoderEvent::SessionFinished {
                outcome: Outcome::Failed,
                at: Utc::now(),
            },
        );
        return Err(CoderError::Validation(feedback));
    }
    let notes = Some(
        pipeline
            .results
            .iter()
            .map(|r| format!("{}: {}", r.id, r.verdict.summary))
            .collect::<Vec<_>>()
            .join("; "),
    );
    Ok(VerifierOutcome {
        notes,
        results: pipeline.results.clone(),
    })
}

/// The verdict layer's working state: the outcome/summary the worker and verifiers produced, to
/// be mutated by the gate or critic, plus their recorded outputs.
struct VerdictState {
    outcome: Outcome,
    summary: String,
    critic_verdict: Option<CriticVerdict>,
    gate_votes: Vec<liberado_coder_core::GateVoteRecord>,
}

/// The judgment layer, on top of the deterministic verifiers above. Two shapes:
///
/// * gate enabled  — a remembered gatekeeper plus a quorum of cold reviewers, adjudicated by the
///   kernel (`liberado_session::CompletionGate`). Fail-closed.
/// * gate disabled — the legacy single critic, unchanged.
///
/// Both are skipped when the worker already failed or changed nothing: there is no claim to
/// dispute, and asking a reviewer to bless an empty diff only burns tokens.
async fn apply_judgment(
    providers: &dyn CoderProviderFactory,
    request: &CoderRunRequest,
    verifier_results: &[liberado_coder_core::NamedVerdict],
    files_changed: &[String],
    events: &trace::EventLog,
    state: &mut VerdictState,
) -> Result<(), CoderError> {
    let reviewable = state.outcome != Outcome::Failed && !files_changed.is_empty();
    if reviewable && request.config.gate.enabled {
        let gate_outcome =
            completion_gate::run_gate(providers, request, verifier_results, events).await?;
        state.gate_votes = completion_gate::flatten_votes(&gate_outcome);
        let verdict = match &gate_outcome.verdict {
            liberado_session::GateVerdict::Approved => CriticVerdict::Acceptable,
            liberado_session::GateVerdict::Refuted { issues } => {
                // Belt and braces: `run`'s attempt loop also derives Failed from a
                // `NeedsRevision` verdict, so this assignment is not the only thing standing
                // between a refutation and a Succeeded result. It is kept so `run_attempt`'s
                // own return value is self-consistent — a caller reading it directly (evals,
                // future single-attempt callers) must never see Succeeded next to a refuted
                // verdict. `critic_verdict`, not `outcome`, is the signal that drives retry.
                state.outcome = Outcome::Failed;
                state.summary = format!(
                    "{}; completion gate refused ({} of {} votes refuting): {}",
                    state.summary,
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
        state.critic_verdict = Some(verdict);
    } else if reviewable && roles::critic_enabled(request) {
        // `None` is an abstention — the reviewer returned nothing usable. The run keeps the
        // verdict the deterministic verifiers already gave it; `critic_verdict` stays `None`
        // so no consumer can mistake silence for approval.
        if let Some(verdict) = critic::run_critic(providers, request, events).await? {
            trace::push_event(
                events,
                CoderEvent::CriticVerdict {
                    verdict: verdict.clone(),
                    at: Utc::now(),
                },
            );
            if let CriticVerdict::NeedsRevision { issues } = &verdict {
                state.outcome = Outcome::Failed;
                state.summary = format!(
                    "{}; critic requested revision: {}",
                    state.summary,
                    issues.join("; ")
                );
            }
            state.critic_verdict = Some(verdict);
        } else {
            state.summary = format!("{}; critic abstained (no usable response)", state.summary);
        }
    }
    Ok(())
}

/// When the pipeline fails and a cargo-test verifier is among the failures, check whether every
/// failing test already exists in `baseline_failures`. If they do, the test verifier is treated as
/// passing — the agent did not introduce the failure.
///
/// `baseline_failures` must be computed externally (e.g. by running `cargo test` against the
/// base commit); this function is the comparison step. Separated so the comparison logic is
/// testable without a live Rust workspace.
///
/// Failures are parsed from the verifier's `log_excerpt` with
/// [`liberado_coder_sandbox::failure_identities`], the same parsing the preflight gate uses, so
/// the two cannot disagree about what counts as a test name.
pub(crate) fn soften_pre_existing_test_failures(
    pipeline: &liberado_coder_core::PipelineResult,
    baseline_failures: &std::collections::BTreeSet<String>,
) -> liberado_coder_core::PipelineResult {
    use liberado_coder_core::{Verdict, VerdictStatus};
    use liberado_coder_sandbox::{OPAQUE_FAILURE, failure_identities};

    // Only look at cargo-test; a failed cargo-check or nonempty-diff is always the agent's fault.
    let test_idx = pipeline
        .results
        .iter()
        .position(|r| r.id == "cargo-test" && !r.verdict.is_pass());
    let Some(idx) = test_idx else {
        return pipeline.clone();
    };

    let current_log = pipeline.results[idx]
        .verdict
        .log_excerpt
        .as_deref()
        .unwrap_or("");
    let current_failures: std::collections::BTreeSet<String> = failure_identities(current_log)
        .into_iter()
        .filter(|f| f != OPAQUE_FAILURE)
        .collect();

    if current_failures.is_empty() {
        return pipeline.clone();
    }

    let all_pre_existing = current_failures
        .iter()
        .all(|f| baseline_failures.contains(f));
    if !all_pre_existing {
        return pipeline.clone();
    }

    tracing::info!(
        count = current_failures.len(),
        "all cargo-test failures are pre-existing; treating test verifier as passing"
    );

    let mut adjusted = pipeline.clone();
    adjusted.results[idx].verdict = Verdict::pass(format!(
        "cargo test: {} pre-existing failure(s) (not new)",
        current_failures.len()
    ));

    // Recompute overall: still fail if any other verifier failed.
    let new_overall = adjusted.results.iter().fold(VerdictStatus::Pass, |acc, r| {
        if acc == VerdictStatus::Fail {
            acc
        } else if r.verdict.status == VerdictStatus::Fail {
            VerdictStatus::Fail
        } else if r.verdict.status == VerdictStatus::Error {
            VerdictStatus::Error
        } else {
            acc
        }
    });
    adjusted.overall = new_overall;
    if new_overall == VerdictStatus::Pass {
        adjusted.combined_signature = None;
        adjusted.combined_findings.clear();
    }

    adjusted
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
        CoderBackend, CoderRoleConfig, CoderRunConfig, CoderTask, CoderTrace, CommandPolicy,
        LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec, WorkspaceRef,
    };
    use liberado_provider::{CompletionResponse, MockProvider, ProviderError, ToolInvocation};
    use serde_json::json;

    /// Retrying a full disk reproduces the full disk. The budget is better spent saying so.
    #[test]
    fn an_infrastructure_failure_is_not_retried() {
        let msg = "FAILURE_CLASS: infrastructure\nFAILURE_SIGNATURE: sig\n\
                   REPAIR_HINT: The build environment failed, not your change.";
        assert!(!is_retryable(&CoderError::Validation(msg.to_string())));
    }

    /// The guard must be narrow. An ordinary validation failure is still worth another attempt,
    /// or this change quietly turns every recoverable run into a single-shot one.
    #[test]
    fn ordinary_validation_failures_are_still_retried() {
        assert!(is_retryable(&CoderError::Validation(
            "FAILURE_CLASS: command_failed\nFINDINGS:\n- cargo exited 101".to_string()
        )));
        assert!(
            !is_retryable(&CoderError::NoChanges),
            "a read-only exhausted attempt must not start another identical NoChanges retry"
        );
        assert!(
            is_stuck_error(&CoderError::NoChanges),
            "NoChanges is still stuck: the pack must ask a human, not treat it as a crash"
        );
    }

    /// A git repo with one committed file. Identity is set explicitly because `user.email` /
    /// `user.name` exist on every dev machine and on no CI runner.
    fn reviewable_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git available");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "--quiet"]);
        run(&["config", "user.email", "test@liberado.local"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("tracked.rs"), "fn old() {}\n").unwrap();
        run(&["add", "tracked.rs"]);
        run(&["commit", "--quiet", "-m", "base"]);
        dir
    }

    /// The critic used to be handed untracked file *names* and no content, so it reported that it
    /// could not see the file — while 334 lines of it sat on disk. A review of a change that omits
    /// its largest part is not a review.
    #[tokio::test]
    async fn the_critic_sees_the_content_of_a_new_file() {
        let dir = reviewable_repo();
        std::fs::write(dir.path().join("added.rs"), "fn brand_new() -> u8 { 7 }\n").unwrap();

        let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
        assert!(diff.contains("added.rs"), "{diff}");
        assert!(
            diff.contains("fn brand_new() -> u8 { 7 }"),
            "the new file's content must reach the reviewer, not only its name: {diff}"
        );
    }

    /// Tracked edits must not be displaced by the untracked section.
    #[tokio::test]
    async fn the_critic_still_sees_tracked_edits() {
        let dir = reviewable_repo();
        std::fs::write(dir.path().join("tracked.rs"), "fn changed() {}\n").unwrap();
        std::fs::write(dir.path().join("added.rs"), "fn brand_new() {}\n").unwrap();

        let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
        assert!(
            diff.contains("fn changed()"),
            "tracked edit missing: {diff}"
        );
        assert!(diff.contains("fn brand_new()"), "new file missing: {diff}");
    }

    /// A binary file must be named but not inlined — a stray NUL would corrupt the transcript the
    /// reviewer reads.
    #[tokio::test]
    async fn a_binary_new_file_is_named_but_not_inlined() {
        let dir = reviewable_repo();
        std::fs::write(dir.path().join("blob.bin"), [0u8, 159, 146, 150]).unwrap();

        let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
        assert!(diff.contains("blob.bin"), "{diff}");
        assert!(diff.contains("(binary)"), "{diff}");
        assert!(!diff.contains('\0'), "no NUL may reach the transcript");
    }

    /// A clean tree must still read as clean.
    #[tokio::test]
    async fn a_clean_tree_is_reported_as_an_empty_diff() {
        let dir = reviewable_repo();
        let diff = workspace_diff(&dir.path().to_string_lossy()).await.unwrap();
        assert_eq!(diff, "(empty diff)", "{diff}");
    }

    fn role() -> CoderRoleConfig {
        CoderRoleConfig {
            model: "mock".to_string(),
            prompt_path: None,
            prompt: Some("Edit the repo and report when done.".to_string()),
            temperature: None,
            max_tokens: None,
            max_turns: Some(6),
            reasoning: None,
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
            reasoning: None,
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
                session_critic: Default::default(),
                prompt_dir: None,
                edit: Default::default(),
                workspace_build: Default::default(),
                offered_tools: None,
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

    /// The trace gap, reproduced.
    ///
    /// A real run (`lib-18ca8ea9645d75d0-15412`) put 122 tool calls on the wire and 76 into trace
    /// files. The missing 46 belonged to the attempt that ended on `critic returned empty content`
    /// — an error that leaves [`critic::run_critic`] through a `?` sitting before the write, so the
    /// whole event log went out with it.
    ///
    /// The attempt that fails unexpectedly is the one whose trace is worth reading, and it was the
    /// one guaranteed not to have one.
    #[tokio::test]
    async fn an_unhandled_error_still_writes_its_trace() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        // Write, then run out of script. Provider exhaustion is an unhandled error: nothing in
        // the attempt path expects it, so it unwinds through the `?` operators this test exists
        // to cover.
        //
        // This test originally reproduced the *empty critic response*, which was the real
        // production failure at the time. That path now abstains rather than erroring
        // (an absent reviewer is not a verdict), so it no longer produces an unhandled error and
        // could not carry this test's claim any more.
        let script = [write_then_report()[0].clone()];
        let provider = Arc::new(MockProvider::with_script("mock", script));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        let traces = dir.path().join("traces");
        request.config.trace_dir = Some(traces.to_string_lossy().into_owned());
        // Enable the critic: a prompt is what turns the role on.
        request.config.critic.prompt = Some("Review the diff.".to_string());

        let err = backend
            .run(request)
            .await
            .expect_err("an unhandled provider error must still fail the run");
        assert!(
            err.to_string().contains("exhausted"),
            "wrong failure reproduced: {err}"
        );

        let written: Vec<_> = std::fs::read_dir(&traces)
            .expect("trace dir must exist even though the attempt died")
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .collect();
        assert!(
            !written.is_empty(),
            "the attempt that crashed is the one whose trace matters most"
        );

        let trace: CoderTrace =
            serde_json::from_str(&std::fs::read_to_string(written[0].path()).unwrap()).unwrap();

        // The tool calls that were being lost.
        assert!(
            trace
                .events
                .iter()
                .any(|e| matches!(e, CoderEvent::ToolFinished { .. })),
            "the work the attempt did must survive its failure: {:?}",
            trace.events
        );
        // And why it died, which is the whole point of keeping it.
        let aborted = trace.events.iter().find_map(|e| match e {
            CoderEvent::SessionAborted { error, .. } => Some(error.clone()),
            _ => None,
        });
        assert!(
            aborted.is_some_and(|e| e.contains("exhausted")),
            "the trace must say what killed the attempt: {:?}",
            trace.events
        );
    }

    /// A trace is a diagnostic, so failing to write one must not fail a run that succeeded.
    ///
    /// The write used to sit behind a `?`. On the machine where the disk filled, that would have
    /// discarded a completed run because its *diagnostic* could not be saved — the same mistake
    /// #119 fixed for `cargo`: the disk being full is not a verdict on the change.
    #[tokio::test]
    async fn a_run_survives_a_trace_it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");

        // A *file* where the trace directory should be, so `create_dir_all` cannot succeed.
        // Kept outside the workspace: a blocker written inside it is an untracked change, and the
        // run would then legitimately report it as one.
        let elsewhere = tempfile::tempdir().unwrap();
        let blocker = elsewhere.path().join("not-a-dir");
        std::fs::write(&blocker, "in the way").unwrap();
        request.config.trace_dir = Some(blocker.join("traces").to_string_lossy().into_owned());

        let result = backend
            .run(request)
            .await
            .expect("an unwritable trace directory must not fail the run");
        assert_eq!(result.files_changed, vec!["hello.txt"]);
        assert!(
            result.trace_path.is_none(),
            "no trace was written, so the result must not claim one"
        );
    }

    /// A body that handles its own failure must not also be reported as a crash. A trace claiming
    /// both a decision and an unhandled error describes neither.
    #[tokio::test]
    async fn a_handled_failure_is_not_relabelled_as_an_abort() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        // Report success while changing nothing: the body detects this and fails it deliberately.
        let script = [CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "report-1",
            liberado_executor::SUBMIT_REPORT_TOOL,
            json!({
                "outcome": "succeeded",
                "summary": "did nothing",
                "artifacts": [],
                "new_high_signal_facts": [],
                "follow_up": null
            }),
        )])];
        let provider = Arc::new(MockProvider::with_script("mock", script));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        let traces = dir.path().join("traces");
        request.config.trace_dir = Some(traces.to_string_lossy().into_owned());

        let _ = backend.run(request).await;

        // Attempt 0 only. `run_attempts` retries a `NoChanges` failure, and the retry exhausts the
        // mock script — a genuine unhandled error, correctly recorded as an abort. Asserting over
        // every file would be asserting that the fix does not work.
        let attempt_zero = std::fs::read_dir(&traces)
            .expect("trace dir")
            .filter_map(|e| e.ok())
            .find(|e| {
                let file_name = e.file_name();
                let name = file_name.to_string_lossy();
                // MVL / execution siblings are `{session}.mvl.jsonl` and share the attempt
                // infix. The CoderEvent document is the `.json` file.
                name.contains("-attempt-0-") && name.ends_with(".json")
            })
            .expect("a handled failure still writes a trace");

        let trace: CoderTrace =
            serde_json::from_str(&std::fs::read_to_string(attempt_zero.path()).unwrap()).unwrap();
        assert!(
            trace
                .events
                .iter()
                .any(|e| matches!(e, CoderEvent::SessionFinished { .. })),
            "the body's own verdict must be what the trace records: {:?}",
            trace.events
        );
        assert!(
            !trace
                .events
                .iter()
                .any(|e| matches!(e, CoderEvent::SessionAborted { .. })),
            "a deliberate failure is not an abort: {:?}",
            trace.events
        );
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
            reasoning: None,
        };

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.critic_verdict, Some(CriticVerdict::Acceptable));
    }

    /// A reviewer that says nothing has not judged the change.
    ///
    /// This destroyed two completed runs. Both had finished their work and passed the
    /// deterministic verifiers; both were filed `Failed` because the provider returned an empty
    /// body. `critic returned empty content` is a fault in the reviewer, not a verdict on the diff.
    #[tokio::test]
    async fn an_empty_critic_response_does_not_discard_the_run() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let mut script = write_then_report().to_vec();
        script.push(CompletionResponse {
            content: None,
            tool_calls: Vec::new(),
            finish_reason: liberado_provider::FinishReason::Stop,
            usage: None,
        });
        let provider = Arc::new(MockProvider::with_script("mock", script));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.critic.prompt = Some("Review the diff strictly.".to_string());

        let result = backend
            .run(request)
            .await
            .expect("an absent reviewer must not fail a finished run");
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(
            result.critic_verdict, None,
            "silence must not be recorded as a verdict"
        );
        assert!(
            result.summary.contains("abstained"),
            "the abstention must be visible in the summary: {}",
            result.summary
        );
    }

    /// Some OpenRouter routes reject `json_schema` even though they accept `json_object`. The
    /// critic must retry without the shape constraint, then keep the completed run and its verdict.
    #[tokio::test]
    async fn a_schema_rejecting_critic_falls_back_to_plain_json() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let provider = Arc::new(MockProvider::with_script("mock", write_then_report()));
        provider.push_error(ProviderError::InvalidRequest(
            "This response_format type is unavailable now".to_string(),
        ));
        provider.push(CompletionResponse::text(r#"{"quality":"acceptable"}"#));
        let backend = LiberadoLoopBackend::new(Arc::clone(&provider) as Arc<dyn Provider>);
        let mut request = request(dir.path(), "HEAD");
        request.config.critic.prompt = Some("Review the diff strictly.".to_string());

        let result = backend
            .run(request)
            .await
            .expect("a critic format fallback must not discard a completed run");
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.critic_verdict, Some(CriticVerdict::Acceptable));

        let requests = provider.received_requests();
        assert_eq!(
            requests.len(),
            4,
            "two coder turns plus two critic attempts"
        );
        assert!(
            requests[2].has_json_schema(),
            "first critic request keeps the schema"
        );
        assert!(
            !requests[3].has_json_schema(),
            "fallback must request plain JSON after a schema rejection"
        );
    }

    /// Same rule for a response that arrives but cannot be parsed. A reviewer that answers in
    /// prose has also not produced a verdict.
    #[tokio::test]
    async fn an_unparseable_critic_response_abstains_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let mut script = write_then_report().to_vec();
        script.push(CompletionResponse::text("Looks fine to me, ship it!"));
        let provider = Arc::new(MockProvider::with_script("mock", script));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.critic.prompt = Some("Review the diff strictly.".to_string());

        let result = backend.run(request).await.expect("must not fail the run");
        assert_eq!(result.outcome, Outcome::Succeeded);
        assert_eq!(result.critic_verdict, None);
    }

    /// The guard must stay narrow: a reviewer that *does* answer still gates the run.
    #[tokio::test]
    async fn a_real_revision_request_still_fails_the_attempt() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let mut script = write_then_report().to_vec();
        script.push(CompletionResponse::text(
            r#"{"quality":"needs_revision","issues":["no tests"]}"#,
        ));
        let provider = Arc::new(MockProvider::with_script("mock", script));
        let backend = LiberadoLoopBackend::new(provider);
        let mut request = request(dir.path(), "HEAD");
        request.config.critic.prompt = Some("Review the diff strictly.".to_string());
        request.config.progress.max_attempts = 1;

        let result = backend.run(request).await.unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        assert!(matches!(
            result.critic_verdict,
            Some(CriticVerdict::NeedsRevision { .. })
        ));
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
            reasoning: None,
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
            reasoning: None,
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
            reasoning: None,
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
    async fn a_no_changes_attempt_is_not_retried() {
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
            reasoning: None,
        });

        let err = backend.run(request).await.expect_err("NoChanges must stop");
        assert!(
            matches!(err, CoderError::NoChanges),
            "a read-only exhausted attempt must not start another identical retry: {err}"
        );
        let roles: Vec<String> = calls
            .lock()
            .unwrap()
            .iter()
            .map(|(role, _)| role.clone())
            .collect();
        assert_eq!(
            roles,
            vec!["coder".to_string()],
            "repair must not run after a NoChanges stall"
        );
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

#[cfg(test)]
mod disposition_tests {
    use super::derive_dispositions;
    use crate::soften_pre_existing_test_failures;
    use liberado_coder_core::{
        Disposition, Finding, FindingKind, NamedVerdict, PipelineResult, Verdict, VerdictStatus,
    };

    fn raised(pairs: &[(u32, &str)]) -> Vec<(u32, String)> {
        pairs.iter().map(|(a, s)| (*a, s.to_string())).collect()
    }

    /// An issue raised early and gone by the end was answered. Reporting it as open would train
    /// a reader to skip the section, which is the only way this mechanism can actually fail.
    #[test]
    fn an_issue_absent_from_the_final_verdict_is_fixed() {
        let findings = derive_dispositions(&raised(&[(0, "the test does not bind")]), &[]);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].disposition, Disposition::Fixed);
        assert_eq!(findings[0].first_seen_attempt, 0);
    }

    /// The case the whole feature exists for: a finding still standing when the run filed.
    #[test]
    fn an_issue_in_the_final_verdict_is_outstanding() {
        let findings = derive_dispositions(
            &raised(&[(0, "still broken")]),
            &["still broken".to_string()],
        );
        assert_eq!(findings[0].disposition, Disposition::Outstanding);
    }

    /// A run that answered two of three complaints must show exactly that, not "clean" and not
    /// "three problems".
    #[test]
    fn a_mixed_run_reports_both_kinds() {
        let findings =
            derive_dispositions(&raised(&[(0, "a"), (0, "b"), (1, "c")]), &["c".to_string()]);
        let outstanding: Vec<&str> = findings
            .iter()
            .filter(|f| f.disposition == Disposition::Outstanding)
            .map(|f| f.issue.as_str())
            .collect();
        assert_eq!(outstanding, vec!["c"]);
        assert_eq!(findings.len(), 3);
    }

    /// The same complaint restated across attempts is one complaint, dated to when it first
    /// appeared — otherwise a stubborn issue inflates into a list and looks like several.
    #[test]
    fn a_repeated_issue_is_one_finding_dated_to_its_first_appearance() {
        let findings = derive_dispositions(
            &raised(&[
                (0, "same complaint"),
                (1, "same complaint"),
                (2, "same complaint"),
            ]),
            &["same complaint".to_string()],
        );
        assert_eq!(findings.len(), 1, "got {findings:?}");
        assert_eq!(findings[0].first_seen_attempt, 0);
        assert_eq!(findings[0].disposition, Disposition::Outstanding);
    }

    #[test]
    fn a_run_with_no_findings_produces_none() {
        assert!(derive_dispositions(&[], &[]).is_empty());
    }

    // ── soften_pre_existing_test_failures ─────────────────────────────────

    fn test_failure_log(test_names: &[&str]) -> String {
        let mut log = String::from("running 3 tests\n");
        for name in test_names {
            log.push_str(&format!("test {name} ... FAILED\n"));
        }
        log.push_str("test result: FAILED. 0 passed; 3 failed; 0 ignored\n");
        log
    }

    fn pipeline_with_test_verdict(
        test_status: VerdictStatus,
        test_log: Option<&str>,
    ) -> PipelineResult {
        PipelineResult {
            overall: if test_status == VerdictStatus::Pass {
                VerdictStatus::Pass
            } else {
                VerdictStatus::Fail
            },
            results: vec![
                NamedVerdict {
                    id: "nonempty-diff".into(),
                    kind: "git_nonempty_diff".into(),
                    verdict: Verdict::pass("non-empty diff"),
                },
                NamedVerdict {
                    id: "cargo-check".into(),
                    kind: "command".into(),
                    verdict: Verdict::pass("cargo exited 0"),
                },
                NamedVerdict {
                    id: "cargo-test".into(),
                    kind: "command".into(),
                    verdict: if test_status == VerdictStatus::Pass {
                        Verdict::pass("cargo exited 0")
                    } else {
                        Verdict::fail(
                            "cargo exited 101",
                            vec![Finding {
                                check_id: "cargo-test".into(),
                                kind: FindingKind::CommandFailed,
                                message: "cargo test exited 101".into(),
                                detail: None,
                            }],
                            test_log.map(|s| s.to_string()),
                        )
                    },
                },
            ],
            combined_findings: if test_status == VerdictStatus::Pass {
                vec![]
            } else {
                vec![Finding {
                    check_id: "cargo-test".into(),
                    kind: FindingKind::CommandFailed,
                    message: "cargo test exited 101".into(),
                    detail: None,
                }]
            },
            combined_signature: None,
        }
    }

    fn bset(items: &[&str]) -> std::collections::BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A failing cargo-test verifier whose failures all exist in the baseline is softened.
    #[test]
    fn pre_existing_test_failures_are_treated_as_passing() {
        let log = test_failure_log(&["foo::test_bar", "foo::test_baz"]);
        let pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));
        assert!(!pipeline.is_pass(), "pipeline starts as failing");

        let baseline = bset(&["foo::test_bar", "foo::test_baz"]);
        let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

        assert!(
            adjusted.is_pass(),
            "all failures are pre-existing; pipeline must pass"
        );
        assert_eq!(
            adjusted.results[2].verdict.status,
            VerdictStatus::Pass,
            "cargo-test verifier must be softened to Pass"
        );
    }

    /// New failures that do not appear in the baseline keep the pipeline failing.
    #[test]
    fn new_test_failures_are_not_softened() {
        let log = test_failure_log(&["foo::test_new_failure"]);
        let pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));
        assert!(!pipeline.is_pass());

        let baseline = bset(&["foo::test_old_failure"]);
        let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

        assert!(
            !adjusted.is_pass(),
            "only pre-existing failures should be softened"
        );
        assert_eq!(
            adjusted.results[2].verdict.status,
            VerdictStatus::Fail,
            "new failure must stay failing"
        );
    }

    /// A mix where some failures are pre-existing and some are new keeps the pipeline failing.
    #[test]
    fn mixed_pre_existing_and_new_failures_stay_failing() {
        let log = test_failure_log(&["foo::old", "foo::new"]);
        let pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));

        let baseline = bset(&["foo::old"]);
        let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

        assert!(
            !adjusted.is_pass(),
            "new failures with pre-existing ones must stay failing"
        );
    }

    /// An empty log excerpt with no parseable failures leaves the pipeline unchanged.
    #[test]
    fn a_test_failure_with_no_parseable_test_names_is_unchanged() {
        let pipeline =
            pipeline_with_test_verdict(VerdictStatus::Fail, Some("error: could not compile\n"));
        let baseline = bset(&["anything"]);
        let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);
        assert!(!adjusted.is_pass(), "opaque failure must not be forgiven");
        assert_eq!(adjusted.results[2].verdict.status, VerdictStatus::Fail,);
    }

    /// A pipeline with no cargo-test verifier is a no-op.
    #[test]
    fn absence_of_cargo_test_verifier_is_a_noop() {
        let pipeline = PipelineResult {
            overall: VerdictStatus::Fail,
            results: vec![NamedVerdict {
                id: "cargo-check".into(),
                kind: "command".into(),
                verdict: Verdict::fail("failed", vec![], None),
            }],
            combined_findings: vec![],
            combined_signature: None,
        };
        let baseline = bset(&["anything"]);
        let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);
        assert!(!adjusted.is_pass());
        assert_eq!(adjusted.results.len(), 1, "pipeline must be unchanged");
    }

    /// When a non-cargo-test verifier also fails, the overall stays failing even if test failures
    /// are all pre-existing.
    #[test]
    fn another_verifier_failing_keeps_overall_failing() {
        let log = test_failure_log(&["foo::test_bar"]);
        let mut pipeline = pipeline_with_test_verdict(VerdictStatus::Fail, Some(&log));
        // Add a failed cargo-check too.
        pipeline.results[1] = NamedVerdict {
            id: "cargo-check".into(),
            kind: "command".into(),
            verdict: Verdict::fail("cargo check exited 1", vec![], None),
        };
        pipeline.overall = VerdictStatus::Fail;

        let baseline = bset(&["foo::test_bar"]);
        let adjusted = soften_pre_existing_test_failures(&pipeline, &baseline);

        assert!(
            !adjusted.is_pass(),
            "cargo-check still fails, so overall must be Fail"
        );
        assert_eq!(
            adjusted.results[2].verdict.status,
            VerdictStatus::Pass,
            "cargo-test was softened, but cargo-check was not"
        );
    }
}

#[cfg(test)]
#[path = "lib_survivor_tests.rs"]
mod survivor_tests;
