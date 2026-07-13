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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use liberado_coder_core::{
    CoderBackend, CoderRoleConfig, CoderRunConfig, CoderRunRequest, CoderTask, CommandPolicy,
    FreezeAuthority, GoalContract, GoalContractDraft, IntakeOutcome, IntakeQuestion,
    LIBERADO_LOOP_BACKEND, PathPolicy, ProgressPolicy, SandboxSpec, VerifierSpec, WorkspaceRef,
};
use liberado_common::{Capability, Outcome};
use liberado_provider::Provider;
use liberado_session::{
    CODING_DOMAIN, DomainPackRunner, GoalResult, GoalSpec, InputChannel, InputOutcome, PackContext,
    PackError, SessionEvent, SessionEventKind, TerminalKind, TurnAuthor,
};
use tokio::sync::mpsc::Sender;

use crate::LiberadoLoopBackend;
use crate::intake_session::{IntakeAnswer, run_intake};

/// Runs coding goals via [`LiberadoLoopBackend`], intake-first.
pub struct CodingSessionPack {
    backend: LiberadoLoopBackend,
    /// The intake model. Held separately from the backend because intake is a *different phase*
    /// with a different job: it reasons about the goal, it does not touch the workspace.
    provider: Arc<dyn Provider>,
    /// Default workspace when payload.workspace_root is absent (temp parent for demos).
    default_workspace_parent: PathBuf,
}

impl CodingSessionPack {
    pub fn new(provider: Arc<dyn Provider>, default_workspace_parent: PathBuf) -> Self {
        Self {
            backend: LiberadoLoopBackend::new(provider.clone()),
            provider,
            default_workspace_parent,
        }
    }

    pub fn with_backend(
        backend: LiberadoLoopBackend,
        provider: Arc<dyn Provider>,
        default_workspace_parent: PathBuf,
    ) -> Self {
        Self {
            backend,
            provider,
            default_workspace_parent,
        }
    }
}

/// How the intake phase ended.
#[derive(Debug)]
enum IntakePhase {
    /// The human accepted a draft; the contract is frozen and authoritative.
    Frozen(Box<GoalContract>),
    /// The human rejected the draft. Nothing is built — a rejected plan is not a failure.
    Rejected,
    /// Clarify rounds were exhausted without reaching a contract (§3.4 step 5). The last partial
    /// draft rides along so the human isn't handed a blank page.
    NeedsReview(Option<Box<GoalContractDraft>>),
    /// Nobody answered within the idle budget.
    IdleExpired(Duration),
}

/// The human's verdict on a draft contract (§3.4 step 3b).
enum FreezeReply {
    Accept,
    Reject,
    /// Anything that isn't a plain accept/reject is treated as a *revision request* — free text
    /// fed straight back into intake as another answer. "Edit" needs no separate mechanism.
    Revise(String),
    IdleExpired(Duration),
}

/// Intake knobs (`verifiers.md` §3.4). Resolved from the session profile's opaque `overrides`
/// (S6) and then the per-session `payload`, which wins — a profile sets the default posture, one
/// session may deviate.
struct IntakeSettings {
    enabled: bool,
    max_clarify_rounds: u32,
}

impl IntakeSettings {
    fn resolve(overrides: &serde_json::Value, payload: &serde_json::Value) -> Self {
        let get = |key: &str| -> Option<&serde_json::Value> {
            payload
                .get("intake")
                .and_then(|t| t.get(key))
                .or_else(|| overrides.get("intake").and_then(|t| t.get(key)))
        };
        Self {
            enabled: get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            max_clarify_rounds: get("max_clarify_rounds")
                .and_then(|v| v.as_u64())
                .unwrap_or(3) as u32,
        }
    }
}

impl CodingSessionPack {
    /// The intake phase: clarify → draft → human freeze (`verifiers.md` §3.4).
    ///
    /// Bounded on purpose — this is a contract negotiation, not an open-ended therapist loop. It
    /// gives up after `max_clarify_rounds` and hands back the last partial draft rather than
    /// grinding on a goal it cannot pin down.
    #[allow(clippy::too_many_arguments)]
    async fn run_intake_phase(
        &self,
        session_id: &str,
        goal: &GoalSpec,
        ctx: &PackContext<'_>,
        settings: &IntakeSettings,
        events: &Sender<SessionEvent>,
        inputs: &mut InputChannel,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<IntakePhase, PackError> {
        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "intake".into(),
                    model: self.provider.model().to_string(),
                },
            ))
            .await;

        let context = goal
            .payload
            .get("context")
            .and_then(|v| v.as_str())
            .filter(|c| !c.trim().is_empty());

        let mut answers: Vec<IntakeAnswer> = Vec::new();
        let mut rounds: u32 = 0;

        loop {
            let outcome = run_intake(&*self.provider, &goal.description, &answers, context)
                .await
                .map_err(|e| PackError::Failed(format!("intake: {e}")))?;

            match outcome {
                IntakeOutcome::ReadyForFreeze { draft, rationale } => {
                    match self
                        .confirm_freeze(session_id, ctx, &draft, &rationale, events, inputs, cancel)
                        .await?
                    {
                        FreezeReply::Accept => {
                            // Freeze stamps the contract with a content hash, so the coding worker
                            // downstream cannot quietly alter the gates it will be judged against.
                            let contract =
                                GoalContract::freeze(session_id, draft, FreezeAuthority::Human)
                                    .map_err(|e| {
                                        PackError::Setup(format!("freeze rejected the draft: {e}"))
                                    })?;
                            let _ = events
                                .send(SessionEvent::new(
                                    session_id,
                                    SessionEventKind::RoleFinished {
                                        role: "intake".into(),
                                    },
                                ))
                                .await;
                            return Ok(IntakePhase::Frozen(Box::new(contract)));
                        }
                        FreezeReply::Reject => return Ok(IntakePhase::Rejected),
                        FreezeReply::IdleExpired(d) => return Ok(IntakePhase::IdleExpired(d)),
                        FreezeReply::Revise(text) => {
                            rounds += 1;
                            if rounds > settings.max_clarify_rounds {
                                return Ok(IntakePhase::NeedsReview(Some(Box::new(draft))));
                            }
                            // A revision is just more human input — no separate "edit" machinery.
                            answers.push(IntakeAnswer {
                                question_id: "revision".into(),
                                answer: text,
                            });
                        }
                    }
                }

                IntakeOutcome::NeedsClarification {
                    questions,
                    partial_draft,
                } => {
                    rounds += 1;
                    // Out of rounds, or the model asked nothing while still not being ready — in
                    // either case it cannot converge, so stop instead of looping forever.
                    if rounds > settings.max_clarify_rounds || questions.is_empty() {
                        return Ok(IntakePhase::NeedsReview(partial_draft.map(Box::new)));
                    }
                    for q in &questions {
                        match self
                            .ask(
                                session_id,
                                ctx,
                                events,
                                inputs,
                                cancel,
                                question_prompt(q),
                                q.options.clone(),
                            )
                            .await?
                        {
                            Some(text) => answers.push(IntakeAnswer {
                                question_id: q.id.clone(),
                                answer: text,
                            }),
                            None => {
                                return Ok(IntakePhase::IdleExpired(
                                    goal.max_idle_secs
                                        .map(Duration::from_secs)
                                        .unwrap_or_default(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Show the draft contract and get the human's verdict (§3.4 step 3b, §3.7 item 3).
    #[allow(clippy::too_many_arguments)]
    async fn confirm_freeze(
        &self,
        session_id: &str,
        ctx: &PackContext<'_>,
        draft: &GoalContractDraft,
        rationale: &str,
        events: &Sender<SessionEvent>,
        inputs: &mut InputChannel,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
    ) -> Result<FreezeReply, PackError> {
        let prompt = render_draft(draft, rationale);
        let options = vec!["accept".to_string(), "reject".to_string()];
        match self
            .ask(session_id, ctx, events, inputs, cancel, prompt, options)
            .await?
        {
            None => Ok(FreezeReply::IdleExpired(Duration::default())),
            Some(text) => Ok(match text.trim().to_ascii_lowercase().as_str() {
                // Exact matches only. A revision like "add a test for the parser" begins with "a";
                // prefix-matching it as "accept" would silently freeze a contract the human was
                // in the middle of changing.
                "accept" | "a" | "y" | "yes" | "ok" => FreezeReply::Accept,
                "reject" | "r" | "n" | "no" | "cancel" => FreezeReply::Reject,
                _ => FreezeReply::Revise(text),
            }),
        }
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
    if dir.join(".git").exists() {
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
    }
}

/// The prompt shown for one clarifying question — `affects` included so the human can see *why*
/// it's being asked, not just what.
fn question_prompt(q: &IntakeQuestion) -> String {
    if q.affects.trim().is_empty() {
        q.prompt.clone()
    } else {
        format!("{}\n(affects: {})", q.prompt, q.affects.trim())
    }
}

/// Render a draft contract for human review. This is the freeze UI: what gets built, and — the
/// part that actually matters — what it will be *judged* against.
fn render_draft(draft: &GoalContractDraft, rationale: &str) -> String {
    let mut s = String::from("Draft contract — review before I build anything.\n\n");
    s.push_str(&format!("Goal: {}\n", draft.description));

    let section = |s: &mut String, title: &str, items: &[String]| {
        if !items.is_empty() {
            s.push_str(&format!("\n{title}:\n"));
            for i in items {
                s.push_str(&format!("  - {i}\n"));
            }
        }
    };
    section(&mut s, "Success criteria", &draft.success_criteria);

    if !draft.verifiers.is_empty() {
        s.push_str("\nVerifiers (the machine gates this will be judged against):\n");
        for v in &draft.verifiers {
            s.push_str(&format!("  - {}\n", verifier_label(v)));
        }
    }
    section(&mut s, "Out of scope", &draft.out_of_scope);
    section(&mut s, "Assumed (not asked)", &draft.assumed_defaults);

    if !rationale.trim().is_empty() {
        s.push_str(&format!("\nWhy these checks: {}\n", rationale.trim()));
    }
    s.push_str(
        "\nReply \"accept\" to freeze this and start building, \"reject\" to abandon, \
         or just describe what to change.",
    );
    s
}

fn verifier_label(v: &VerifierSpec) -> String {
    match v {
        VerifierSpec::PathsExist { id, paths } => {
            format!("{id}: these paths must exist — {}", paths.join(", "))
        }
        VerifierSpec::PathsAbsent { id, paths } => {
            format!("{id}: these paths must NOT exist — {}", paths.join(", "))
        }
        VerifierSpec::ContentContains {
            id,
            path,
            must_include,
        } => format!("{id}: {path} must contain — {}", must_include.join(", ")),
        VerifierSpec::Command {
            id, program, args, ..
        } => format!("{id}: `{program} {}` must pass", args.join(" ")),
        VerifierSpec::GitNonemptyDiff { id } => {
            format!("{id}: the working tree must actually change")
        }
    }
}

#[async_trait]
impl DomainPackRunner for CodingSessionPack {
    fn domain_id(&self) -> &str {
        CODING_DOMAIN
    }

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

        // ── Phase 2: build against the frozen contract ──────────────────────────────────────
        let workspace = goal
            .payload
            .get("workspace_root")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let dir = self
                    .default_workspace_parent
                    .join(format!("goal-{session_id}"));
                let _ = std::fs::create_dir_all(&dir);
                init_git_repo(&dir);
                dir
            });

        let model = goal
            .payload
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("session-coder")
            .to_string();

        let prompt = goal
            .payload
            .get("coder_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or(
                "You are Liberado's coding worker. Inspect, edit with tools, then submit_report.",
            )
            .to_string();

        let max_turns = if goal.max_turns > 0 {
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
                repair: Some(role),
                sandbox: SandboxSpec::HostLocal,
                command_policy: CommandPolicy::default(),
                validation_command: None,
                verifiers: Vec::new(),
                verify_policy: Default::default(),
                path_policy: PathPolicy::default(),
                progress: ProgressPolicy {
                    max_attempts: 2,
                    ..ProgressPolicy::default()
                },
            },
            attempt: 0,
            prior_feedback: Vec::new(),
        };

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

        let _ = events
            .send(SessionEvent::new(
                session_id,
                SessionEventKind::RoleStarted {
                    role: "coder".into(),
                    model,
                },
            ))
            .await;

        // Race coding run against cancel (best-effort; LiberadoLoopBackend is not yet cancel-aware).
        let run_fut = self.backend.run(request);
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
                Ok(GoalResult {
                    terminal: if ok {
                        TerminalKind::Succeeded
                    } else {
                        TerminalKind::Failed
                    },
                    summary: r.summary,
                    artifacts: r.files_changed,
                    diagnostics: r.diagnostics,
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
                Ok(GoalResult {
                    terminal: TerminalKind::Failed,
                    summary: msg,
                    artifacts: vec![],
                    diagnostics: serde_json::json!({"error": "coder_backend"}),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_provider::{CompletionResponse, MockProvider};
    use liberado_session::HumanInput;
    use tokio::sync::mpsc;

    fn ready_json(description: &str) -> String {
        serde_json::to_string(&IntakeOutcome::ReadyForFreeze {
            draft: GoalContractDraft {
                description: description.into(),
                success_criteria: vec!["add and list work".into()],
                verifiers: vec![VerifierSpec::PathsExist {
                    id: "paths".into(),
                    paths: vec!["src/main.rs".into()],
                }],
                out_of_scope: vec!["network".into()],
                assumed_defaults: vec!["Rust".into()],
                domain_hint: Some("coding".into()),
                verify_profile: None,
            },
            rationale: "the stack is clear".into(),
        })
        .unwrap()
    }

    const CLARIFY_JSON: &str = r#"{
        "status": "needs_clarification",
        "questions": [{"id":"stack","prompt":"Rust or Node?","options":["Rust","Node"],"affects":"verify profile"}]
    }"#;

    /// A pack whose intake model replays `script`, plus a pre-loaded human input channel. Human
    /// answers are buffered, so the pack consumes them in order at each await point.
    fn harness(
        script: Vec<&str>,
        human: Vec<&str>,
    ) -> (
        CodingSessionPack,
        Sender<SessionEvent>,
        mpsc::Receiver<SessionEvent>,
        InputChannel,
        tokio::sync::watch::Receiver<bool>,
        // The cancel *sender* must be handed back and kept alive: dropping it makes
        // `cancel.changed()` resolve immediately, which races `inputs.recv()` in the pack's
        // `select!` and cancels the session at random. Tests bind it to keep the session live.
        tokio::sync::watch::Sender<bool>,
    ) {
        let provider = Arc::new(MockProvider::with_script(
            "mock",
            script
                .into_iter()
                .map(CompletionResponse::text)
                .collect::<Vec<_>>(),
        ));
        let pack = CodingSessionPack::new(provider, std::env::temp_dir());

        let (ev_tx, ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        for h in human {
            in_tx.try_send(HumanInput::new(h)).unwrap();
        }
        drop(in_tx); // an await with no answer left closes rather than hanging the test
        let inputs = InputChannel::new(in_rx, None);
        let (c_tx, c_rx) = tokio::sync::watch::channel(false);
        (pack, ev_tx, ev_rx, inputs, c_rx, c_tx)
    }

    fn goal(description: &str) -> GoalSpec {
        GoalSpec {
            id: None,
            description: description.into(),
            success_criteria: vec![],
            domain: liberado_session::DomainHint::Coding,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({}),
        }
    }

    fn settings(max_clarify_rounds: u32) -> IntakeSettings {
        IntakeSettings {
            enabled: true,
            max_clarify_rounds,
        }
    }

    /// A real (in-memory) store with session `s1` open, plus the grant a `PackContext` borrows.
    /// Turns the pack records actually land here, so a test can assert the transcript — which is the
    /// whole point of S7's dialogue becoming turns rather than events.
    struct Transcript {
        store: Arc<liberado_session::GoalSessionStore>,
        grant: liberado_session::SessionGrant,
    }

    impl Transcript {
        async fn open() -> Self {
            let store = Arc::new(liberado_session::GoalSessionStore::new());
            // The session must be open under the SAME id the pack records against, or every turn is
            // dropped on the floor — which is exactly what a store does with a turn for a session it
            // has never heard of.
            let mut spec = goal("make a todo cli");
            spec.id = Some("s1".into());
            liberado_session::SessionRecordStore::insert(
                store.as_ref(),
                liberado_session::GoalSessionRecord::new(spec),
            )
            .await;
            Self {
                store,
                grant: liberado_session::SessionGrant::default(),
            }
        }
        fn ctx(&self) -> PackContext<'_> {
            PackContext::new(&self.grant, self.store.clone(), "s1")
        }
    }

    fn prompts(rx: &mut mpsc::Receiver<SessionEvent>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let SessionEventKind::AwaitingInput { prompt, .. } = ev.kind {
                out.push(prompt);
            }
        }
        out
    }

    #[tokio::test]
    async fn intake_clarifies_then_freezes_on_accept() {
        // The S7 spine: the model asks, the human answers, the model drafts, the human accepts, and
        // the draft becomes an authoritative contract — all before a line of code is written.
        let (pack, ev_tx, mut ev_rx, mut inputs, mut cancel, _cancel_tx) = harness(
            vec![CLARIFY_JSON, &ready_json("Build a todo CLI")],
            vec!["Rust", "accept"],
        );

        let tr = Transcript::open().await;
        let ctx = tr.ctx();
        let phase = pack
            .run_intake_phase(
                "s1",
                &goal("make a todo cli"),
                &ctx,
                &settings(3),
                &ev_tx,
                &mut inputs,
                &mut cancel,
            )
            .await
            .unwrap();

        let contract = match phase {
            IntakePhase::Frozen(c) => c,
            other => panic!("expected a frozen contract, got {other:?}"),
        };
        assert_eq!(contract.draft.description, "Build a todo CLI");
        assert_eq!(contract.frozen_by, FreezeAuthority::Human);
        assert!(
            !contract.draft.verifiers.is_empty(),
            "the contract must carry the machine gates — that is the point of freezing it"
        );
        assert!(!contract.content_hash.is_empty());

        // ...and the negotiation that produced it is a **conversation**, recorded as turns.
        //
        // This used to be events only, which meant the intake Q&A was invisible to `chat-search`
        // (it matches message nodes) and the session could not be forked (forking copies a node
        // prefix, and an event log has no `parent_id`). Every question the pack asked is here.
        let turns = tr.store.turns("s1").await;
        assert!(
            turns
                .iter()
                .any(|(who, what)| *who == TurnAuthor::Assistant && what.contains("Rust or Node?")),
            "the clarifying question must be in the transcript, not just on the event bus: {turns:#?}"
        );
        assert!(
            turns
                .iter()
                .any(|(who, what)| *who == TurnAuthor::Assistant
                    && what.contains("Build a todo CLI")),
            "so must the draft contract the human was asked to accept: {turns:#?}"
        );

        // The human saw the question (with its `affects`), then the draft for review.
        let seen = prompts(&mut ev_rx);
        assert_eq!(seen.len(), 2, "one clarify prompt + one freeze prompt");
        assert!(seen[0].contains("Rust or Node?") && seen[0].contains("verify profile"));
        assert!(seen[1].contains("Draft contract") && seen[1].contains("src/main.rs"));
    }

    #[tokio::test]
    async fn rejecting_the_draft_builds_nothing() {
        let (pack, ev_tx, _ev_rx, mut inputs, mut cancel, _cancel_tx) =
            harness(vec![&ready_json("Build a todo CLI")], vec!["reject"]);

        let tr = Transcript::open().await;
        let ctx = tr.ctx();
        let phase = pack
            .run_intake_phase(
                "s1",
                &goal("make a todo cli"),
                &ctx,
                &settings(3),
                &ev_tx,
                &mut inputs,
                &mut cancel,
            )
            .await
            .unwrap();
        assert!(matches!(phase, IntakePhase::Rejected));
    }

    #[tokio::test]
    async fn free_text_is_a_revision_not_an_accept() {
        // The trap this guards: "add a test for the parser" starts with 'a'. Prefix-matching it as
        // "accept" would freeze a contract the human was in the middle of changing. It must feed
        // back into intake as another answer, producing a fresh draft to review.
        let (pack, ev_tx, mut ev_rx, mut inputs, mut cancel, _cancel_tx) = harness(
            vec![&ready_json("v1"), &ready_json("v2 (revised)")],
            vec!["add a test for the parser", "accept"],
        );

        let tr = Transcript::open().await;
        let ctx = tr.ctx();
        let phase = pack
            .run_intake_phase(
                "s1",
                &goal("make a todo cli"),
                &ctx,
                &settings(3),
                &ev_tx,
                &mut inputs,
                &mut cancel,
            )
            .await
            .unwrap();

        match phase {
            IntakePhase::Frozen(c) => assert_eq!(
                c.draft.description, "v2 (revised)",
                "the revision must produce a second draft, not freeze the first"
            ),
            other => panic!("expected the revised contract to freeze, got {other:?}"),
        }
        assert_eq!(
            prompts(&mut ev_rx).len(),
            2,
            "the human reviewed two drafts"
        );
    }

    #[tokio::test]
    async fn exhausting_clarify_rounds_stops_and_hands_back_the_partial_draft() {
        // Bounded, not an open-ended therapist loop (verifiers.md §3.4 step 5): a model that keeps
        // asking gets cut off, and the human is handed whatever was worked out rather than nothing.
        let (pack, ev_tx, _ev_rx, mut inputs, mut cancel, _cancel_tx) =
            harness(vec![CLARIFY_JSON, CLARIFY_JSON], vec!["Rust"]);

        let tr = Transcript::open().await;
        let ctx = tr.ctx();
        let phase = pack
            .run_intake_phase(
                "s1",
                &goal("something vague"),
                &ctx,
                &settings(1),
                &ev_tx,
                &mut inputs,
                &mut cancel,
            )
            .await
            .unwrap();
        assert!(
            matches!(phase, IntakePhase::NeedsReview(_)),
            "expected NeedsReview once the round budget ran out, got {phase:?}"
        );
    }

    #[test]
    fn payload_intake_settings_beat_the_profile_overrides() {
        // A profile sets the default posture; a single session may deviate from it.
        let overrides =
            serde_json::json!({ "intake": { "enabled": true, "max_clarify_rounds": 3 } });
        let payload = serde_json::json!({ "intake": { "enabled": false } });
        let s = IntakeSettings::resolve(&overrides, &payload);
        assert!(!s.enabled, "payload wins over the profile");
        assert_eq!(
            s.max_clarify_rounds, 3,
            "keys the payload didn't set still fall back to the profile"
        );

        // Defaults: intake on, 3 rounds — intake-first is the whole point of a coding session.
        let d = IntakeSettings::resolve(&serde_json::json!({}), &serde_json::json!({}));
        assert!(d.enabled);
        assert_eq!(d.max_clarify_rounds, 3);
    }

    #[test]
    fn the_draft_review_shows_what_it_will_be_judged_against() {
        let draft = GoalContractDraft {
            description: "Build a todo CLI".into(),
            success_criteria: vec!["add and list work".into()],
            verifiers: vec![
                VerifierSpec::PathsExist {
                    id: "paths".into(),
                    paths: vec!["src/main.rs".into()],
                },
                VerifierSpec::GitNonemptyDiff { id: "diff".into() },
            ],
            out_of_scope: vec!["network".into()],
            assumed_defaults: vec!["Rust".into()],
            domain_hint: None,
            verify_profile: None,
        };
        let out = render_draft(&draft, "the stack is clear");
        assert!(out.contains("Build a todo CLI"));
        assert!(out.contains("add and list work"));
        assert!(
            out.contains("src/main.rs"),
            "the machine gates must be visible before freeze, not after"
        );
        assert!(out.contains("must actually change"));
        assert!(out.contains("network") && out.contains("Rust"));
        assert!(out.contains("the stack is clear"));
        assert!(out.contains("accept") && out.contains("reject"));
    }
}
