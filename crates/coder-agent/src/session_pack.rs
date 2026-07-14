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
}

impl CodingSessionPack {
    pub fn new(provider: Arc<dyn Provider>, default_workspace_parent: PathBuf) -> Self {
        Self {
            backend: Arc::new(LiberadoLoopBackend::new(provider.clone())),
            provider,
            default_workspace_parent,
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
        }
    }
}

/// How many times the coherence checker (S7-c) may send a draft back to the model before it gives up
/// and shows the human what it could not fix.
///
/// Deliberately small, and deliberately **not** the human's clarify budget. Two attempts is enough
/// for a model that made an honest slip; a model that fails twice is not going to succeed on the
/// fifth, and the useful thing to do at that point is hand the human the draft *with the findings
/// attached* — not keep talking to the model until the session dies.
const MAX_COHERENCE_REDRAFTS: u32 = 2;

/// Rebuild the intake `answers` from a session's transcript (E6-c).
///
/// The transcript of a coding session in intake reads:
///
/// ```text
/// user       <the goal>                  ← recorded by the kernel, not an answer
/// assistant  <a clarifying question>     ← recorded by the pack's `ask`
/// user       <the human's answer>        ← recorded by the kernel on human input
/// assistant  <a draft contract>
/// user       accept / or a revision
/// ```
///
/// So every **user** turn after the first is an answer, and the assistant turn immediately before it
/// is the question it answered. `run_intake` renders these as `"- {question}: {answer}"`, so pairing
/// them this way gives the model a *better* prompt than the original run did — which used an opaque
/// question id.
///
/// **What this does not recover, stated plainly.** Machine-generated intake feedback (a revision
/// request the coherence checker sent back, S7-c) was never a *turn*, so it is not here. The model
/// may therefore phrase its next question slightly differently than it did before the restart. That
/// is acceptable **only** because intake ends at a draft contract the human must accept: an
/// approximate reconstruction that lands in front of a human for approval is safe. The same
/// approximation applied to a build loop — which edits files — would not be, and that is exactly why
/// [`CodingSessionPack::can_resume`] says no once the build has started.
fn answers_from_transcript(turns: &[(TurnAuthor, String)]) -> Vec<IntakeAnswer> {
    let mut answers = Vec::new();
    let mut last_question: Option<&str> = None;
    let mut seen_goal = false;

    for (author, content) in turns {
        match author {
            TurnAuthor::Assistant => last_question = Some(content.as_str()),
            TurnAuthor::User => {
                if !seen_goal {
                    // The first user turn is the goal itself, not an answer to anything.
                    seen_goal = true;
                    continue;
                }
                answers.push(IntakeAnswer {
                    question_id: last_question
                        .map(|q| q.lines().next().unwrap_or(q).trim().to_string())
                        .unwrap_or_else(|| "answer".into()),
                    answer: content.clone(),
                });
                last_question = None;
            }
            _ => {}
        }
    }
    answers
}

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

        // E6-c: on a resume, the transcript is our only memory of the negotiation. Rebuild the
        // answers from it so we do not re-ask what has already been answered. Empty on a fresh
        // session, so this costs nothing in the normal case.
        let mut answers: Vec<IntakeAnswer> = answers_from_transcript(&ctx.prior_turns().await);
        if !answers.is_empty() {
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::Progress {
                        message: format!(
                            "resumed: picking the contract negotiation back up with {} prior \
                             answer(s)",
                            answers.len()
                        ),
                    },
                ))
                .await;
        }
        let mut rounds: u32 = 0;
        // Redrafts spent on the coherence checker, budgeted separately from the human's clarify
        // rounds: they are the *model's* mistakes, and spending a person's budget on them means a
        // stubborn model can talk the human out of ever being consulted.
        let mut coherence_redrafts: u32 = 0;

        loop {
            let outcome = run_intake(&*self.provider, &goal.description, &answers, context)
                .await
                .map_err(|e| PackError::Failed(format!("intake: {e}")))?;

            match outcome {
                IntakeOutcome::ReadyForFreeze { draft, rationale } => {
                    // S7-c: a draft that contradicts *itself* never reaches the human. This is the
                    // model's mistake to fix, not something to spend a person's attention noticing
                    // in a wall of prose at the end of a workday — send it straight back with the
                    // finding. (It bit us twice in one live session: `verify_profile` re-added
                    // gates the model's own out-of-scope prose said it had dropped, and the model
                    // could not fix it by editing the verifier list, only by clearing the profile.)
                    let conflicts = liberado_coder_core::contradictions(&draft);
                    // Its OWN budget, separate from the human's clarify rounds — and on exhaustion
                    // it **gives up and asks the human**, it does not kill the session.
                    //
                    // Both halves of that were wrong when this shipped, and one live run found it:
                    // the redrafts consumed `max_clarify_rounds`, so three false contradictions
                    // (see `GENERIC` in `coherence.rs`) burned the human's entire budget and the
                    // session died with `needs human review` — having never once asked the human
                    // anything. A machine check that can terminate a session the human never saw is
                    // strictly worse than no check at all: it converts "the linter is wrong" into
                    // "the work is gone". The linter's failure mode must be *deferring to the
                    // human*, never *overruling* them.
                    if !conflicts.is_empty() && coherence_redrafts < MAX_COHERENCE_REDRAFTS {
                        coherence_redrafts += 1;
                        let detail = conflicts
                            .iter()
                            .map(|c| format!("- {}", c.message))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let _ = events
                            .send(SessionEvent::new(
                                session_id,
                                SessionEventKind::Progress {
                                    message: format!(
                                        "draft contract contradicts itself ({} finding(s)) — \
                                         redrafting",
                                        conflicts.len()
                                    ),
                                },
                            ))
                            .await;
                        answers.push(IntakeAnswer {
                            question_id: "coherence".into(),
                            answer: format!(
                                "Your draft contract contradicts itself. A contract is frozen and \
                                 binding — the worker cannot argue with it — so it must be \
                                 coherent before I accept it. Fix these and re-draft:\n{detail}"
                            ),
                        });
                        continue;
                    }

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
        // Verifier PROVENANCE, not just the list. `verify_profile` silently appends gates the model
        // did not write, so its prose ("no clippy") could sincerely contradict its own binding
        // verifier list — and the human reads the prose. Saying where each gate came from is what
        // makes that visible; without it, the only clue is that the build fails on something nobody
        // asked for.
        let injected = liberado_coder_core::profile_injected_ids(draft);
        s.push_str("\nVerifiers (the machine gates this will be judged against):\n");
        for v in &draft.verifiers {
            let origin = if injected.iter().any(|id| id == v.id()) {
                format!(
                    "   [added by verify_profile = \"{}\", not written for this goal]",
                    draft.verify_profile.as_deref().unwrap_or("?")
                )
            } else {
                String::new()
            };
            s.push_str(&format!("  - {}{origin}\n", verifier_label(v)));
        }
        if !injected.is_empty() {
            s.push_str(&format!(
                "  ({} of these came from the profile. To drop them, clear `verify_profile` — \
                 removing them from the list will not work, the profile re-adds them.)\n",
                injected.len()
            ));
        }
    }
    section(&mut s, "Out of scope", &draft.out_of_scope);
    section(&mut s, "Assumed (not asked)", &draft.assumed_defaults);

    // S7-c findings. Warnings are the human's judgement to make. *Contradictions* reaching this
    // point mean the checker sent the draft back to the model, the model failed to fix it, and the
    // checker gave up — so the human is now the backstop and must be told plainly, rather than
    // handed a draft that will refuse to freeze with no explanation.
    let findings = liberado_coder_core::contract_conflicts(draft);
    let (contradictions, warnings): (Vec<_>, Vec<_>) = findings
        .into_iter()
        .partition(|f| f.severity == liberado_coder_core::Severity::Contradiction);

    if !contradictions.is_empty() {
        s.push_str(
            "\n⛔ This draft contradicts itself, and the model could not fix it. It will NOT \
             freeze as-is — tell me what to change:\n",
        );
        for c in &contradictions {
            s.push_str(&format!("  - {}\n", c.message));
        }
    }
    if !warnings.is_empty() {
        s.push_str("\n⚠ Check these before you accept:\n");
        for w in &warnings {
            s.push_str(&format!("  - {}\n", w.message));
        }
    }

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

    /// Resumable while still negotiating the contract; **not** once the build has started (E6-c).
    ///
    /// The line is drawn exactly where irreversibility begins. Intake reasons about the goal and
    /// touches nothing, so re-deriving it from the transcript is safe even though the
    /// reconstruction is approximate — it ends at a draft the human must accept, and an approximate
    /// draft in front of a human for approval harms nobody. The build *edits files*. Re-running it
    /// from an approximate reconstruction, with no checkpoint of what the last attempt already did,
    /// would redo real work against a workspace that is no longer in the state the reconstruction
    /// assumes. So the answer there is no, and the session stays parked and says so, rather than
    /// resuming optimistically and quietly corrupting a workspace.
    ///
    /// (The remaining work — a workspace checkpoint that would make the build resumable too — is
    /// E6-c's deferred half. The coder workspace is already a git repo, so a commit is the obvious
    /// suspend point; it is a design pass, not a line of code, and it is not this slice.)
    async fn can_resume(&self, ctx: &PackContext<'_>) -> bool {
        let started_building = ctx.prior_events().await.iter().any(|e| {
            matches!(
                &e.kind,
                SessionEventKind::RoleStarted { role, .. } if role == "coder"
            )
        });
        !started_building
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
            let run_fut = self.backend.run(request.clone());
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
    use liberado_coder_core::{CoderError, CoderRunResult};
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

    /// A backend that fails the first attempt and succeeds on the next, recording every request it
    /// was handed. The recording is the point: it is the only way to prove a human's mid-build
    /// answer actually reaches the *backend* rather than merely being narrated to the event bus.
    struct ScriptedBackend {
        seen: Arc<std::sync::Mutex<Vec<CoderRunRequest>>>,
        fail_attempts: u32,
    }

    #[async_trait]
    impl CoderBackend for ScriptedBackend {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
            let attempt = request.attempt;
            self.seen.lock().unwrap().push(request);
            let failed = attempt < self.fail_attempts;
            Ok(CoderRunResult {
                backend: "scripted".into(),
                outcome: if failed {
                    Outcome::Failed
                } else {
                    Outcome::Succeeded
                },
                summary: if failed {
                    "verifier `tests` failed".into()
                } else {
                    "green".into()
                },
                files_changed: vec![],
                validation_notes: None,
                critic_verdict: None,
                trace_path: None,
                diagnostics: serde_json::json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_failed_build_asks_the_human_and_retries_with_their_answer() {
        // The point of the whole exercise (one-execution-engine E5): a goal-pursuing session that
        // hits a wall stops, asks, waits, and then *uses the answer*. Recording the guidance in the
        // transcript and failing anyway would look identical on the event bus and be worthless — so
        // this asserts the guidance arrives in the backend's second attempt as `prior_feedback`.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = Arc::new(ScriptedBackend {
            seen: seen.clone(),
            fail_attempts: 1,
        });
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

        let (ev_tx, _ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        in_tx
            .try_send(HumanInput::new("pin serde to 1.0 and rerun"))
            .unwrap();
        drop(in_tx);
        let inputs = InputChannel::new(in_rx, None);
        let (_c_tx, cancel) = tokio::sync::watch::channel(false);

        let workspace = std::env::temp_dir().join("liberado-e5-retry-test");
        std::fs::create_dir_all(&workspace).unwrap();

        // Intake off (this test is about the *build* loop), AskHuman on (so the pack may stop).
        let mut g = goal("make a todo cli");
        g.payload = serde_json::json!({
            "workspace_root": workspace.to_string_lossy(),
            "intake": { "enabled": false },
        });

        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = g.clone();
        spec.id = Some("s1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant {
            capabilities: [Capability::AskHuman].into_iter().collect(),
            ..Default::default()
        };
        let ctx = PackContext::new(&grant, store.clone(), "s1");

        let out = pack
            .run("s1", &g, &ctx, ev_tx, inputs, cancel)
            .await
            .unwrap();

        let requests = seen.lock().unwrap().clone();
        assert_eq!(
            requests.len(),
            2,
            "the pack must actually re-run the backend after the human answers, not just record \
             the answer and fail: {out:?}"
        );
        assert_eq!(requests[0].attempt, 0);
        assert_eq!(requests[1].attempt, 1, "the retry is a new attempt");
        assert!(
            requests[1]
                .prior_feedback
                .iter()
                .any(|f| f.contains("pin serde to 1.0")),
            "the human's guidance must reach the backend as feedback: {:#?}",
            requests[1].prior_feedback
        );
        assert_eq!(
            out.terminal,
            TerminalKind::Succeeded,
            "the guided retry succeeded, so the session succeeded"
        );
    }

    /// A backend that gets **stuck** (`Err(NoChanges)`) on its first attempt rather than returning a
    /// failed verdict, then succeeds once it has been told something. This is the shape a real run
    /// produces when the model cannot make progress — and the shape `ScriptedBackend` never made,
    /// which is precisely why the ask seam shipped on the `Ok` path only and the live test caught it.
    struct StuckBackend {
        seen: Arc<std::sync::Mutex<Vec<CoderRunRequest>>>,
    }

    #[async_trait]
    impl CoderBackend for StuckBackend {
        fn name(&self) -> &str {
            "stuck"
        }
        async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
            let attempt = request.attempt;
            self.seen.lock().unwrap().push(request);
            if attempt == 0 {
                return Err(CoderError::NoChanges);
            }
            Ok(CoderRunResult {
                backend: "stuck".into(),
                outcome: Outcome::Succeeded,
                summary: "green".into(),
                files_changed: vec![],
                validation_notes: None,
                critic_verdict: None,
                trace_path: None,
                diagnostics: serde_json::json!({}),
            })
        }
    }

    #[tokio::test]
    async fn a_stuck_build_asks_the_human_instead_of_dying_silently() {
        // The live test's actual failure: the coder built a working CLI, hit a gate it had no way to
        // satisfy, could not make further progress, and the backend returned Err(NoChanges) -- which
        // bypassed the ask entirely and killed the session. The more stuck the pack got, the less
        // able it was to ask for help. A stuck build must ask.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = Arc::new(StuckBackend { seen: seen.clone() });
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

        let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        in_tx
            .try_send(HumanInput::new("the release token is ORCHID-7Q"))
            .unwrap();
        drop(in_tx);
        let inputs = InputChannel::new(in_rx, None);
        let (_c_tx, cancel) = tokio::sync::watch::channel(false);

        let workspace = std::env::temp_dir().join("liberado-e5-stuck-test");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut g = goal("make a todo cli");
        g.payload = serde_json::json!({
            "workspace_root": workspace.to_string_lossy(),
            "intake": { "enabled": false },
        });

        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = g.clone();
        spec.id = Some("s1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant {
            capabilities: [Capability::AskHuman].into_iter().collect(),
            ..Default::default()
        };
        let ctx = PackContext::new(&grant, store.clone(), "s1");

        let out = pack
            .run("s1", &g, &ctx, ev_tx, inputs, cancel)
            .await
            .unwrap();

        assert_eq!(
            prompts(&mut ev_rx).len(),
            1,
            "a stuck backend must ask the human, not die silently"
        );
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), 2, "and then actually retry with the answer");
        assert!(
            requests[1]
                .prior_feedback
                .iter()
                .any(|f| f.contains("ORCHID-7Q")),
            "the answer must reach the backend: {:#?}",
            requests[1].prior_feedback
        );
        assert_eq!(out.terminal, TerminalKind::Succeeded);
    }

    #[tokio::test]
    async fn a_broken_environment_fails_fast_instead_of_paging_you() {
        // The other half of the distinction: no answer you could type fixes a dead sandbox. Asking
        // would be noise, and the ask is only valuable because it is rare.
        struct BrokenBackend;
        #[async_trait]
        impl CoderBackend for BrokenBackend {
            fn name(&self) -> &str {
                "broken"
            }
            async fn run(&self, _r: CoderRunRequest) -> Result<CoderRunResult, CoderError> {
                Err(CoderError::Sandbox("workspace root vanished".into()))
            }
        }
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let pack = CodingSessionPack::with_backend(
            Arc::new(BrokenBackend),
            provider,
            std::env::temp_dir(),
        );

        let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (_in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        let inputs = InputChannel::new(in_rx, None);
        let (_c_tx, cancel) = tokio::sync::watch::channel(false);

        let workspace = std::env::temp_dir().join("liberado-e5-broken-test");
        std::fs::create_dir_all(&workspace).unwrap();
        let mut g = goal("make a todo cli");
        g.payload = serde_json::json!({
            "workspace_root": workspace.to_string_lossy(),
            "intake": { "enabled": false },
        });

        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = g.clone();
        spec.id = Some("s1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant {
            capabilities: [Capability::AskHuman].into_iter().collect(),
            ..Default::default()
        };
        let ctx = PackContext::new(&grant, store.clone(), "s1");

        let out = pack
            .run("s1", &g, &ctx, ev_tx, inputs, cancel)
            .await
            .unwrap();
        assert_eq!(out.terminal, TerminalKind::Failed);
        assert_eq!(
            prompts(&mut ev_rx).len(),
            0,
            "a dead sandbox is not a question for a human"
        );
    }

    #[tokio::test]
    async fn the_ask_budget_bounds_the_retries_so_a_stuck_pack_cannot_interrogate_you() {
        // A pack that may ask whenever it is stuck is worse than one that guesses: it would keep
        // coming back forever. One ask (the default) means one guided retry, then it stops and
        // reports — it does not ask a second time.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let backend = Arc::new(ScriptedBackend {
            seen: seen.clone(),
            fail_attempts: 99, // never succeeds, however much guidance it is given
        });
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let pack = CodingSessionPack::with_backend(backend, provider, std::env::temp_dir());

        let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(64);
        let (in_tx, in_rx) = mpsc::channel::<HumanInput>(16);
        for answer in ["try again", "and again", "and again"] {
            in_tx.try_send(HumanInput::new(answer)).unwrap();
        }
        drop(in_tx);
        let inputs = InputChannel::new(in_rx, None);
        let (_c_tx, cancel) = tokio::sync::watch::channel(false);

        let workspace = std::env::temp_dir().join("liberado-e5-budget-test");
        std::fs::create_dir_all(&workspace).unwrap();

        let mut g = goal("make a todo cli");
        g.payload = serde_json::json!({
            "workspace_root": workspace.to_string_lossy(),
            "intake": { "enabled": false },
        });

        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = g.clone();
        spec.id = Some("s1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant {
            capabilities: [Capability::AskHuman].into_iter().collect(),
            ..Default::default()
        };
        let ctx = PackContext::new(&grant, store.clone(), "s1");

        let out = pack
            .run("s1", &g, &ctx, ev_tx, inputs, cancel)
            .await
            .unwrap();

        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "one ask means the initial attempt plus exactly one guided retry — no more"
        );
        assert_eq!(out.terminal, TerminalKind::Failed);
        assert_eq!(
            prompts(&mut ev_rx).len(),
            1,
            "the human is asked once, not once per failure"
        );
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

    /// The incoherent draft from the live run, reproduced: `verify_profile = "rust-strict"` injects
    /// clippy/fmt while the model's own `out_of_scope` sincerely says it dropped them. This must
    /// never reach the human — it goes straight back to the model, and the human is only ever shown
    /// the coherent redraft.
    #[tokio::test]
    async fn a_self_contradicting_draft_goes_back_to_the_model_not_to_the_human() {
        let incoherent = serde_json::to_string(&IntakeOutcome::ReadyForFreeze {
            draft: GoalContractDraft {
                description: "Build a todo CLI".into(),
                success_criteria: vec!["it works".into()],
                verifiers: vec![],
                out_of_scope: vec!["No clippy or fmt checks.".into()],
                assumed_defaults: vec![],
                domain_hint: Some("coding".into()),
                // The trap: this silently re-adds cargo-clippy and cargo-fmt at expansion time, so
                // the prose above becomes a lie about a list the model never sees.
                verify_profile: Some("rust-strict".into()),
            },
            rationale: "ready".into(),
        })
        .unwrap();

        let (pack, ev_tx, mut ev_rx, mut inputs, mut cancel, _c) = harness(
            vec![&incoherent, &ready_json("Build a todo CLI")],
            vec!["accept"],
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

        assert!(
            matches!(phase, IntakePhase::Frozen(_)),
            "the redraft should freeze: {phase:?}"
        );

        // The human was asked exactly ONCE — for the coherent redraft. They never saw the
        // contradictory one, because catching it is the machine's job, not theirs.
        let seen = prompts(&mut ev_rx);
        assert_eq!(
            seen.len(),
            1,
            "the human must not be shown a draft that contradicts itself: {seen:#?}"
        );
        assert!(
            !seen[0].contains("No clippy or fmt checks"),
            "and certainly not the contradictory one: {}",
            seen[0]
        );
    }

    #[tokio::test]
    async fn freeze_refuses_a_contract_that_contradicts_itself() {
        // Belt and braces: even if a contradictory draft somehow reaches freeze, freeze refuses.
        // Freezing is what makes the gates binding — the worker cannot argue with them — so binding
        // it to something impossible means it obeys, faithfully, into the ground.
        let mut draft = GoalContractDraft {
            description: "Build a todo CLI".into(),
            success_criteria: vec!["it works".into()],
            verifiers: vec![],
            out_of_scope: vec!["No clippy checks.".into()],
            assumed_defaults: vec![],
            domain_hint: None,
            verify_profile: Some("rust-strict".into()),
        };
        liberado_coder_core::sanitize_draft(&mut draft);

        let err = GoalContract::freeze("s1", draft, FreezeAuthority::Human)
            .expect_err("a self-contradictory contract must not become binding");
        assert!(err.contains("contradicts itself"), "{err}");
        assert!(err.contains("cargo-clippy"), "must name the gate: {err}");
    }

    #[test]
    fn the_transcript_rebuilds_the_intake_answers() {
        // The shape a real parked coding session leaves behind. The FIRST user turn is the goal --
        // not an answer to anything -- and getting that wrong would feed the goal back to the model
        // as though it were a reply, which is exactly the kind of off-by-one that produces a
        // confidently wrong second question.
        let turns = vec![
            (TurnAuthor::User, "make a todo cli".to_string()),
            (TurnAuthor::Assistant, "Rust or Node?".to_string()),
            (TurnAuthor::User, "Rust".to_string()),
            (
                TurnAuthor::Assistant,
                "What file path for persistence?".to_string(),
            ),
            (TurnAuthor::User, "todos.json".to_string()),
        ];
        let answers = answers_from_transcript(&turns);
        assert_eq!(answers.len(), 2, "the goal is not an answer: {answers:#?}");
        assert_eq!(answers[0].question_id, "Rust or Node?");
        assert_eq!(answers[0].answer, "Rust");
        assert_eq!(answers[1].question_id, "What file path for persistence?");
        assert_eq!(answers[1].answer, "todos.json");
    }

    #[test]
    fn a_fresh_session_reconstructs_nothing() {
        // The normal case must cost nothing and, above all, must not invent an answer.
        assert!(answers_from_transcript(&[]).is_empty());
        assert!(
            answers_from_transcript(&[(TurnAuthor::User, "make a todo cli".into())]).is_empty(),
            "a session that has only been given its goal has answered nothing"
        );
    }

    #[tokio::test]
    async fn the_coding_pack_will_not_resume_once_the_build_has_started() {
        // The line where irreversibility begins. Intake touches nothing, so an approximate
        // reconstruction is safe -- it ends at a draft a human must accept. The build EDITS FILES,
        // and re-running it from an approximate reconstruction, against a workspace no longer in the
        // state that reconstruction assumes, is how you quietly corrupt someone's work.
        let provider = Arc::new(MockProvider::with_script("mock", vec![]));
        let pack = CodingSessionPack::new(provider, std::env::temp_dir());
        let store = Arc::new(liberado_session::GoalSessionStore::new());
        let mut spec = goal("make a todo cli");
        spec.id = Some("s1".into());
        liberado_session::SessionRecordStore::insert(
            store.as_ref(),
            liberado_session::GoalSessionRecord::new(spec),
        )
        .await;
        let grant = liberado_session::SessionGrant::default();

        let ctx = PackContext::new(&grant, store.clone(), "s1");
        assert!(
            pack.can_resume(&ctx).await,
            "a session still in intake is resumable"
        );

        // The build starts.
        liberado_session::SessionRecordStore::push_event(
            store.as_ref(),
            SessionEvent::new(
                "s1",
                SessionEventKind::RoleStarted {
                    role: "coder".into(),
                    model: "m".into(),
                },
            ),
        )
        .await;

        assert!(
            !pack.can_resume(&ctx).await,
            "once the build has touched the workspace, resume is no longer safe"
        );
    }
}
