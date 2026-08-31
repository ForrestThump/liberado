//! Coding **domain pack** for Liberado's agentic orchestration.
//!
//! This crate composes the shared inner loop (`liberado-executor`) with coding tools, sandbox,
//! deterministic verifiers, progress guards, optional critic, and attempt/repair. It is a domain
//! specialization — not the center of Liberado. See
//! `docs/spec/architecture/agentic-loops.md` and `docs/future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md`.

pub mod assemble;
mod coding_goal;
pub mod cold_review;
mod completion_gate;
mod critic;
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

use async_trait::async_trait;
use chrono::Utc;
use liberado_coder_core::{
    CoderBackend, CoderError, CoderEvent, CoderRoleConfig, CoderRunRequest, CoderRunResult,
    CriticVerdict, LIBERADO_LOOP_BACKEND, resolve_verifier_specs,
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
        // The attempt loop is separated from what comes after it so the session critic reads the
        // *whole* run — including every repair attempt. The repair turns are the ones worth
        // reading: an agent answering review feedback is under the most pressure to say "good
        // catch, fixed" and move on, which is exactly the shape of the failure this looks for.
        let config = request.config.clone();
        let mut result = self.run_attempts(request).await?;
        self.review_session_after_run(&config, &mut result).await;
        Ok(result)
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
            backend: self.name().to_string(),
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
        );
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
        } = Self::prepare_worker_runtime(&request, &session_id, &events, event_preview_max_chars)
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
            backend: self.name().to_string(),
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
#[path = "lib_unit_tests.rs"]
mod unit_tests;

#[cfg(test)]
#[path = "lib_loop_tests.rs"]
mod loop_tests;

#[cfg(test)]
#[path = "lib_disposition_tests.rs"]
mod disposition_tests;

#[cfg(test)]
#[path = "lib_survivor_tests.rs"]
mod survivor_tests;
