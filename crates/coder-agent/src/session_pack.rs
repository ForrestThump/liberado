//! Coding domain pack adapter for [`liberado_session::DomainPackRunner`].
//!
//! Bridges `LiberadoLoopBackend` into the goal-session kernel so TUI/WebUI can drive coding
//! goals without owning the loop. Optional: only used when server registers this pack.
//!
//! # Intake-first (session-focus S7)
//!
//! A coding session does not go straight from a one-line goal to a coding loop. It first runs
//! **intake** (`verifiers.md` §3.4): a thinking model turns the human's rough writeup into either
//! clarifying questions or a **draft contract** (description + success criteria + machine
//! verifiers), the human answers/accepts through the session's human-input channel, and the
//! accepted draft is **frozen** into a [`GoalContract`] before a single line is written.
//!
//! This matters for a reason beyond convenience: the frozen contract supplies the **verifiers** —
//! the machine gates the work is judged against. Before S7 this pack ran with `verifiers: []`, so
//! the coding loop effectively graded its own homework. The contract is what makes the gates
//! *authoritative*, and freeze is what makes them the human's, not the model's.
//!
//! Intake requires [`Capability::AskHuman`] (S6). A session without it — an unattended cron —
//! skips intake and builds directly from the description, exactly as before S7, rather than
//! blocking on questions nobody will answer.

mod build;
mod intake;
mod policies;
mod preflight_hook;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

use intake::{IntakePhase, IntakeSettings};

use async_trait::async_trait;
use liberado_coder_core::CoderBackend;
use liberado_common::Capability;
use liberado_provider::Provider;
use liberado_session::{
    CODING_DOMAIN, DomainPackRunner, GoalResult, GoalSpec, InputChannel, InputOutcome, PackContext,
    PackError, SessionEvent, SessionEventKind, TerminalKind, TurnAuthor,
};
use tokio::sync::mpsc::Sender;

use crate::LiberadoLoopBackend;

/// Runs coding goals via a [`CoderBackend`] (in production, [`LiberadoLoopBackend`]), intake-first.
pub struct CodingSessionPack {
    /// The trait, not the concrete backend: the build loop's behaviour — notably that a human's
    /// mid-build answer actually reaches the *next* attempt — is only testable if a double can
    /// stand in here.
    backend: Arc<dyn CoderBackend>,
    /// The intake model. Held separately from the backend because intake is a *different phase*
    /// with a different job: it reasons about the goal, it does not touch the workspace.
    provider: Arc<dyn Provider>,
    /// Default workspace when payload.workspace_root is absent (temp parent for demos).
    default_workspace_parent: PathBuf,
    /// Hub for S6 fan-out child sessions. Set after the hub is `Arc`'d (see
    /// [`attach_hub`](Self::attach_hub)) — registration order is pack-then-Arc.
    hub: std::sync::Mutex<Option<Arc<liberado_session::GoalSessionHub>>>,
    /// Default concurrency for `payload.subtasks` fan-out
    /// (`tuning.dispatch.max_concurrent_coding_subagents`).
    max_concurrent_coding_subagents: u32,
    /// Default hashline settings from `[coder.hashline]` in tuning.toml.
    hashline: liberado_coder_core::HashlineConfig,
    /// The coder role from `[coder.coder]` — its model and turn ceiling.
    ///
    /// The pack previously passed the literal `"session-coder"` as a model name and its own
    /// 12-turn default, so `[coder.coder]`'s configured model and budget governed nothing here.
    coder_role: liberado_coder_core::CoderRoleConfig,
    /// Completion gate settings from `[coder.gate]` in tuning.toml.
    ///
    /// Defaults to off, exactly as before. What changed is that it is now *reachable*: every
    /// `CoderRunConfig` this pack built previously hardcoded `CoderGateConfig::default()`, so
    /// `[tuning.coder.gate]` parsed, validated, and could not be switched on through the daemon at
    /// any setting.
    gate: liberado_coder_core::CoderGateConfig,
    /// Progress-guard thresholds from `[coder.progress]` in tuning.toml.
    ///
    /// Same story as `gate`: `CoderTuning` has carried a validated `progress` table all along, and
    /// every `CoderRunConfig` built here hardcoded `ProgressPolicy::default()`, so the table could
    /// not change the guard at any setting. These are the thresholds most likely to need tuning per
    /// repo — how many inspect calls a task may spend before the guard calls it a stall depends
    /// entirely on how many files the change spans — and they were the one thing you had to
    /// recompile to adjust.
    progress: liberado_coder_core::ProgressPolicy,
    /// Where durable run traces are written (`[coder] trace_dir`), or `None` to write none.
    ///
    /// Seventh instance of the same defect: `CoderTuning::trace_dir` has a default of
    /// `Some("coder-traces")` and every consumer hardcoded `None`, so the trace facility — which
    /// has a passing test — had never produced a file in production.
    trace_dir: Option<String>,
    /// Trace formats to write (`[coder] trace_formats`). Empty = native only.
    trace_formats: Vec<liberado_coder_core::TraceFormat>,
}

impl CodingSessionPack {
    pub fn new(provider: Arc<dyn Provider>, default_workspace_parent: PathBuf) -> Self {
        Self {
            backend: Arc::new(LiberadoLoopBackend::new(provider.clone())),
            provider,
            default_workspace_parent,
            hub: std::sync::Mutex::new(None),
            max_concurrent_coding_subagents: crate::fanout::DEFAULT_MAX_CONCURRENT_CODING_SUBAGENTS
                as u32,
            hashline: liberado_coder_core::HashlineConfig::default(),
            coder_role: liberado_coder_core::CoderTuning::default().coder,
            gate: liberado_coder_core::CoderGateConfig::default(),
            progress: liberado_coder_core::ProgressPolicy::default(),
            trace_dir: None,
            trace_formats: Vec::new(),
        }
    }

    pub fn with_backend(
        backend: Arc<dyn CoderBackend>,
        provider: Arc<dyn Provider>,
        default_workspace_parent: PathBuf,
    ) -> Self {
        Self {
            backend,
            provider,
            default_workspace_parent,
            hub: std::sync::Mutex::new(None),
            max_concurrent_coding_subagents: crate::fanout::DEFAULT_MAX_CONCURRENT_CODING_SUBAGENTS
                as u32,
            hashline: liberado_coder_core::HashlineConfig::default(),
            coder_role: liberado_coder_core::CoderTuning::default().coder,
            gate: liberado_coder_core::CoderGateConfig::default(),
            progress: liberado_coder_core::ProgressPolicy::default(),
            trace_dir: None,
            trace_formats: Vec::new(),
        }
    }

    /// Resource cap for parallel coding subagents (from tuning). Clamped to ≥ 1 at use site.
    pub fn with_max_concurrent_coding_subagents(mut self, n: u32) -> Self {
        self.max_concurrent_coding_subagents = n.max(1);
        self
    }

    /// Seed hashline edit mode from `[coder.hashline]` (payload/overrides can still override).
    /// Seed the coder role (model + turn ceiling) from `[coder.coder]`.
    pub fn with_coder_role(mut self, role: liberado_coder_core::CoderRoleConfig) -> Self {
        self.coder_role = role;
        self
    }

    /// Replace the backend with one that mints a provider per role, so `CoderRoleConfig::model`
    /// selects a model instead of being ignored.
    ///
    /// `SingleProviderFactory` — what `new` installs — hands back the one daemon provider for
    /// every role regardless of the model asked for. It cannot simply `set_model` on that shared
    /// provider either: the model sits behind a `RwLock` on the trait object, so mutating it would
    /// change the model for every other holder, the chat face agent included.
    pub fn with_provider_factory(
        mut self,
        providers: Arc<dyn crate::CoderProviderFactory>,
    ) -> Self {
        self.backend = Arc::new(crate::LiberadoLoopBackend::with_provider_factory(providers));
        self
    }

    /// Completion-gate settings for sessions this pack runs (`[coder.gate]`).
    ///
    /// Off by default and left that way: the gate costs `1 + fresh_reviewers` extra model calls
    /// per attempt, so it stays opt-in. This only makes the opt-in possible.
    pub fn with_gate(mut self, config: liberado_coder_core::CoderGateConfig) -> Self {
        self.gate = config;
        self
    }

    /// Progress-guard thresholds for sessions this pack runs (`[coder.progress]`).
    /// Where run traces are written for sessions this pack runs (`[coder] trace_dir`).
    pub fn with_trace_dir(mut self, dir: Option<String>) -> Self {
        self.trace_dir = dir;
        self
    }

    /// Which trace formats sessions this pack runs should write (`[coder] trace_formats`).
    pub fn with_trace_formats(mut self, formats: Vec<liberado_coder_core::TraceFormat>) -> Self {
        self.trace_formats = formats;
        self
    }

    pub fn with_progress(mut self, policy: liberado_coder_core::ProgressPolicy) -> Self {
        self.progress = policy;
        self
    }

    pub fn with_hashline(mut self, config: liberado_coder_core::HashlineConfig) -> Self {
        self.hashline = config;
        self
    }

    /// Attach the goal hub so `payload.subtasks` spawns **child goal sessions** rather than
    /// in-process backend workers. Call once after `Arc::new(hub)`.
    pub fn attach_hub(&self, hub: Arc<liberado_session::GoalSessionHub>) {
        *self.hub.lock().expect("coding pack hub mutex") = Some(hub);
    }

    fn hub(&self) -> Option<Arc<liberado_session::GoalSessionHub>> {
        self.hub.lock().expect("coding pack hub mutex").clone()
    }

    /// Emit `AwaitingInput` and block for the answer. `None` = the idle budget expired.
    ///
    /// Every question this pack asks a human goes through here — the clarify rounds *and* the freeze
    /// prompt — which is why the **turn** is recorded here and not at each call site. One choke
    /// point, so no future question can be added that quietly fails to make it into the transcript.
    /// (The human's answer is recorded by the hub, for the same reason.)
    #[allow(clippy::too_many_arguments)]
    async fn ask(
        &self,
        session_id: &str,
        ctx: &PackContext<'_>,
        events: &Sender<SessionEvent>,
        inputs: &mut InputChannel,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
        prompt: String,
        options: Vec<String>,
    ) -> Result<Option<String>, PackError> {
        ctx.record_turn(TurnAuthor::Assistant, prompt.clone()).await;

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::AwaitingInput { prompt, options },
            ))
            .await;

        let outcome = tokio::select! {
            o = inputs.recv() => o,
            _ = cancel.changed() => InputOutcome::Closed,
        };
        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }
        match outcome {
            InputOutcome::Received(input) => Ok(Some(input.text)),
            InputOutcome::IdleExpired(_) => Ok(None),
            InputOutcome::Closed => Err(PackError::Cancelled),
        }
    }
}

#[async_trait]
impl DomainPackRunner for CodingSessionPack {
    fn domain_id(&self) -> &str {
        CODING_DOMAIN
    }

    /// Resumable when:
    /// - still in **intake** (no coder role yet) — rebuild from transcript; or
    /// - **mid-build** and at least one **checkpoint** event exists (S4 / E6-c(b)).
    ///
    /// Without a checkpoint, mid-build resume would re-run tools against an unknown FS state;
    /// with one, we restore files-only then re-enter the build phase.
    async fn can_resume(&self, ctx: &PackContext<'_>) -> bool {
        let events = ctx.prior_events().await;
        let started_building = events.iter().any(|e| {
            matches!(
                &e.kind,
                SessionEventKind::RoleStarted { role, .. }
                    if role == "coder" || role == "coder-fanout"
            )
        });
        if !started_building {
            return true;
        }
        events
            .iter()
            .any(|e| matches!(&e.kind, SessionEventKind::Checkpoint { .. }))
    }
    /// The pack's whole story, in one place: **negotiate a contract, then build against it.**
    ///
    /// This used to be a ~400-line function that also held the workspace setup, the build attempt
    /// loop, the ask seam and the retry logic. The two phases fail differently and are resumable
    /// differently, and burying that under one roof is how the ask seam ended up unreachable from
    /// the one case that most needed it. Each phase now owns its own module.
    async fn run(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        ctx: &PackContext<'_>,
        events: Sender<SessionEvent>,
        mut inputs: InputChannel,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<GoalResult, PackError> {
        if *cancel.borrow() {
            return Err(PackError::Cancelled);
        }

        let prior = ctx.prior_events().await;
        let mid_build_resume = prior.iter().any(|e| {
            matches!(
                &e.kind,
                SessionEventKind::RoleStarted { role, .. }
                    if role == "coder" || role == "coder-fanout"
            )
        });

        // S4 / E6-c(b): mid-build resume restores files from the latest checkpoint, then rebuilds.
        // Restore into the **durable** session worktree (coding-worktrees/{id}) when present —
        // that is where attempt snapshots were taken. Fall back to payload workspace_root for
        // HostLocal / non-git sessions.
        if mid_build_resume {
            if let Some((id, label)) = last_checkpoint(&prior) {
                let workspace =
                    coding_checkpoint_workspace(session_id, goal, &self.default_workspace_parent);
                match liberado_coder_sandbox::ShadowGit::open_or_init(&workspace, session_id) {
                    Ok(sg) => {
                        if let Err(e) = sg.restore(&id).await {
                            return Err(PackError::Failed(format!(
                                "mid-build resume: restore checkpoint {id} failed: {e}"
                            )));
                        }
                        let _ = events
                            .send(SessionEvent::new(
                                session_id,
                                SessionEventKind::Progress {
                                    message: format!(
                                        "mid-build resume: restored checkpoint {label} ({id}) \
                                         into {}",
                                        workspace.display()
                                    ),
                                },
                            ))
                            .await;
                    }
                    Err(e) => {
                        return Err(PackError::Failed(format!(
                            "mid-build resume: open shadow-git failed: {e}"
                        )));
                    }
                }
            }
            // Skip intake — contract negotiation already happened (or was skipped) before park.
            return self
                .run_build_phase(session_id, goal, ctx, None, events, inputs, cancel)
                .await;
        }

        // ── Phase 1: intake (S7) ────────────────────────────────────────────────────────────
        // Asking the human is a capability, not a mode (S6). A session whose grant omits AskHuman
        // has a closed input channel, so intake would deadlock until its idle budget burned — skip
        // it and build directly from the description, which is exactly what this pack did pre-S7.
        let settings = IntakeSettings::resolve(ctx.overrides(), &goal.payload);
        let may_ask = ctx.can(&Capability::AskHuman);

        let contract = if settings.enabled && may_ask {
            match self
                .run_intake_phase(
                    session_id,
                    goal,
                    ctx,
                    &settings,
                    &events,
                    &mut inputs,
                    &mut cancel,
                )
                .await?
            {
                IntakePhase::Frozen(contract) => Some(contract),
                IntakePhase::Rejected => {
                    return Ok(GoalResult {
                        terminal: TerminalKind::Cancelled,
                        summary: "contract rejected — nothing was built".into(),
                        artifacts: vec![],
                        diagnostics: serde_json::json!({ "phase": "intake", "rejected": true }),
                    });
                }
                IntakePhase::NeedsReview(partial) => {
                    let summary = format!(
                        "intake could not reach a contract in {} round(s) — needs human review",
                        settings.max_clarify_rounds
                    );
                    let _ = events
                        .send(SessionEvent::new(
                            session_id,
                            SessionEventKind::Failed {
                                message: summary.clone(),
                            },
                        ))
                        .await;
                    return Ok(GoalResult {
                        terminal: TerminalKind::Failed,
                        summary,
                        artifacts: vec![],
                        diagnostics: serde_json::json!({
                            "phase": "intake",
                            "partial_draft": partial,
                        }),
                    });
                }
                IntakePhase::IdleExpired(d) => {
                    return Ok(GoalResult {
                        terminal: TerminalKind::BudgetExhausted,
                        summary: format!(
                            "no answer to intake after {}s — nothing was built",
                            d.as_secs()
                        ),
                        artifacts: vec![],
                        diagnostics: serde_json::json!({ "phase": "intake", "idle_timeout": true }),
                    });
                }
            }
        } else {
            if settings.enabled && !may_ask {
                let _ = events
                    .send(SessionEvent::new(
                        session_id,
                        SessionEventKind::Progress {
                            message: "intake skipped: this session's grant does not include \
                                      AskHuman, so it cannot ask clarifying questions — building \
                                      directly from the goal description"
                                .into(),
                        },
                    ))
                    .await;
            }
            None
        };

        self.run_build_phase(session_id, goal, ctx, contract, events, inputs, cancel)
            .await
    }
}

fn last_checkpoint(events: &[SessionEvent]) -> Option<(String, String)> {
    events.iter().rev().find_map(|e| match &e.kind {
        SessionEventKind::Checkpoint { id, label, .. } => Some((id.clone(), label.clone())),
        _ => None,
    })
}

/// Workspace root where shadow-git snapshots for this coding session live.
///
/// Prefer the durable session worktree when it exists (build-on-git path); otherwise the
/// authorized `workspace_root` / default goal workspace (HostLocal / non-git).
pub(crate) fn coding_checkpoint_workspace(
    session_id: &str,
    goal: &GoalSpec,
    default_parent: &std::path::Path,
) -> std::path::PathBuf {
    if let Some(sess) = liberado_coder_tools::durable_session_workspace(session_id)
        && sess.exists()
    {
        return sess;
    }
    goal.payload
        .get("workspace_root")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| default_parent.join(format!("goal-{session_id}")))
}
