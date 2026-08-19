//! Phase 1 of a coding session: **intake** — turn a rough goal into a contract a human has frozen.
//!
//! Split out of `session_pack.rs` (2026-07-14), which had grown a ~400-line `run` holding both
//! phases, the ask seam, the retry loop and the workspace setup. The two phases answer different
//! questions and fail in different ways: intake *reasons about the goal and touches nothing*; the
//! build *edits files*. That is the same line `CodingSessionPack::can_resume` draws, so it
//! is the right place to cut.
//!
//! `ask` deliberately stays in the parent: it is the one choke point through which every question
//! this pack asks a human passes, and both phases use it.

use liberado_coder_core::{
    FreezeAuthority, GoalContract, GoalContractDraft, IntakeOutcome, IntakeQuestion, VerifierSpec,
};
use liberado_session::{
    GoalSpec, InputChannel, PackContext, PackError, SessionEvent, SessionEventKind, TurnAuthor,
};
use std::time::Duration;
use tokio::sync::mpsc::Sender;

use super::CodingSessionPack;
use crate::intake_session::{IntakeAnswer, run_intake};

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
/// `CodingSessionPack::can_resume` says no once the build has started.
pub(super) fn answers_from_transcript(turns: &[(TurnAuthor, String)]) -> Vec<IntakeAnswer> {
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

/// How the intake phase ended.
#[derive(Debug)]
pub(super) enum IntakePhase {
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
pub(super) struct IntakeSettings {
    pub(super) enabled: bool,
    pub(super) max_clarify_rounds: u32,
}

impl IntakeSettings {
    pub(super) fn resolve(overrides: &serde_json::Value, payload: &serde_json::Value) -> Self {
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
    /// The intake phase: clarify → draft → human freeze (`verifiers.md` §3.4).
    ///
    /// Bounded on purpose — this is a contract negotiation, not an open-ended therapist loop. It
    /// gives up after `max_clarify_rounds` and hands back the last partial draft rather than
    /// grinding on a goal it cannot pin down.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn run_intake_phase(
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

        let context_owned = build_intake_context(goal);
        let context = context_owned.as_deref();
        let mut state = IntakeLoop {
            // E6-c: on a resume, the transcript is our only memory of the negotiation. Rebuild
            // the answers from it so we do not re-ask what has already been answered. Empty on a
            // fresh session, so this costs nothing in the normal case.
            answers: answers_from_transcript(&ctx.prior_turns().await),
            rounds: 0,
            // Redrafts spent on the coherence checker, budgeted separately from the human's
            // clarify rounds: they are the *model's* mistakes, and spending a person's budget on
            // them means a stubborn model can talk the human out of ever being consulted.
            coherence_redrafts: 0,
        };
        if !state.answers.is_empty() {
            let _ = events
                .send(SessionEvent::new(
                    session_id,
                    SessionEventKind::Progress {
                        message: format!(
                            "resumed: picking the contract negotiation back up with {} prior \
                             answer(s)",
                            state.answers.len()
                        ),
                    },
                ))
                .await;
        }
        let mut c = IntakeCtx {
            session_id,
            goal,
            ctx,
            settings,
            events,
            inputs,
            cancel,
        };

        loop {
            let outcome = run_intake(
                &*self.provider,
                &c.goal.description,
                &state.answers,
                context,
            )
            .await
            .map_err(|e| PackError::Failed(format!("intake: {e}")))?;

            match outcome {
                IntakeOutcome::ReadyForFreeze { draft, rationale } => {
                    match self
                        .handle_ready_for_freeze(&mut c, &mut state, draft, rationale)
                        .await?
                    {
                        IntakeStep::Finish(phase) => return Ok(phase),
                        IntakeStep::Continue => {}
                    }
                }
                IntakeOutcome::NeedsClarification {
                    questions,
                    partial_draft,
                } => {
                    match self
                        .handle_clarification(&mut c, &mut state, questions, partial_draft)
                        .await?
                    {
                        IntakeStep::Finish(phase) => return Ok(phase),
                        IntakeStep::Continue => {}
                    }
                }
            }
        }
    }

    /// A draft ready for human freeze: run the coherence check first (its own budget — the
    /// model's mistakes must not spend the human's clarify rounds), then act on the freeze
    /// verdict. `Some(phase)` ends the phase; `None` loops again.
    async fn handle_ready_for_freeze(
        &self,
        c: &mut IntakeCtx<'_>,
        state: &mut IntakeLoop,
        draft: GoalContractDraft,
        rationale: String,
    ) -> Result<IntakeStep, PackError> {
        // S7-c: a draft that contradicts *itself* never reaches the human. This is the model's
        // mistake to fix, not something to spend a person's attention noticing in a wall of
        // prose at the end of a workday — send it straight back with the finding. (It bit us
        // twice in one live session: `verify_profile` re-added gates the model's own
        // out-of-scope prose said it had dropped, and the model could not fix it by editing the
        // verifier list, only by clearing the profile.)
        let conflicts = liberado_coder_core::contradictions(&draft);
        // Its OWN budget, separate from the human's clarify rounds — and on exhaustion it
        // **gives up and asks the human**, it does not kill the session.
        //
        // Both halves of that were wrong when this shipped, and one live run found it: the
        // redrafts consumed `max_clarify_rounds`, so three false contradictions (see `GENERIC`
        // in `coherence.rs`) burned the human's entire budget and the session died with `needs
        // human review` — having never once asked the human anything. A machine check that can
        // terminate a session the human never saw is strictly worse than no check at all: it
        // converts "the linter is wrong" into "the work is gone". The linter's failure mode must
        // be *deferring to the human*, never *overruling* them.
        if !conflicts.is_empty() && state.coherence_redrafts < MAX_COHERENCE_REDRAFTS {
            state.coherence_redrafts += 1;
            let detail = conflicts
                .iter()
                .map(|c| format!("- {}", c.message))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = c
                .events
                .send(SessionEvent::new(
                    c.session_id,
                    SessionEventKind::Progress {
                        message: format!(
                            "draft contract contradicts itself ({} finding(s)) — redrafting",
                            conflicts.len()
                        ),
                    },
                ))
                .await;
            state.answers.push(IntakeAnswer {
                question_id: "coherence".into(),
                answer: format!(
                    "Your draft contract contradicts itself. A contract is frozen and binding — \
                     the worker cannot argue with it — so it must be coherent before I accept it. \
                     Fix these and re-draft:\n{detail}"
                ),
            });
            return Ok(IntakeStep::Continue);
        }

        match self
            .confirm_freeze(
                c.session_id,
                c.ctx,
                &draft,
                &rationale,
                c.events,
                c.inputs,
                c.cancel,
            )
            .await?
        {
            FreezeReply::Accept => {
                // Freeze stamps the contract with a content hash, so the coding worker downstream
                // cannot quietly alter the gates it will be judged against.
                let contract = GoalContract::freeze(c.session_id, draft, FreezeAuthority::Human)
                    .map_err(|e| PackError::Setup(format!("freeze rejected the draft: {e}")))?;
                let _ = c
                    .events
                    .send(SessionEvent::new(
                        c.session_id,
                        SessionEventKind::RoleFinished {
                            role: "intake".into(),
                        },
                    ))
                    .await;
                Ok(IntakeStep::Finish(IntakePhase::Frozen(Box::new(contract))))
            }
            FreezeReply::Reject => Ok(IntakeStep::Finish(IntakePhase::Rejected)),
            FreezeReply::IdleExpired(d) => Ok(IntakeStep::Finish(IntakePhase::IdleExpired(d))),
            FreezeReply::Revise(text) => {
                state.rounds += 1;
                if state.rounds > c.settings.max_clarify_rounds {
                    return Ok(IntakeStep::Finish(IntakePhase::NeedsReview(Some(
                        Box::new(draft),
                    ))));
                }
                // A revision is just more human input — no separate "edit" machinery.
                state.answers.push(IntakeAnswer {
                    question_id: "revision".into(),
                    answer: text,
                });
                Ok(IntakeStep::Continue)
            }
        }
    }

    /// The model asked for more input: spend one round per question, or give up when the rounds
    /// are exhausted or the model had nothing to ask.
    async fn handle_clarification(
        &self,
        c: &mut IntakeCtx<'_>,
        state: &mut IntakeLoop,
        questions: Vec<IntakeQuestion>,
        partial_draft: Option<GoalContractDraft>,
    ) -> Result<IntakeStep, PackError> {
        state.rounds += 1;
        // Out of rounds, or the model asked nothing while still not being ready — in either case
        // it cannot converge, so stop instead of looping forever.
        if state.rounds > c.settings.max_clarify_rounds || questions.is_empty() {
            return Ok(IntakeStep::Finish(IntakePhase::NeedsReview(
                partial_draft.map(Box::new),
            )));
        }
        for q in &questions {
            match self
                .ask(
                    c.session_id,
                    c.ctx,
                    c.events,
                    c.inputs,
                    c.cancel,
                    question_prompt(q),
                    q.options.clone(),
                )
                .await?
            {
                Some(text) => state.answers.push(IntakeAnswer {
                    question_id: q.id.clone(),
                    answer: text,
                }),
                None => {
                    return Ok(IntakeStep::Finish(IntakePhase::IdleExpired(
                        c.goal
                            .max_idle_secs
                            .map(Duration::from_secs)
                            .unwrap_or_default(),
                    )));
                }
            }
        }
        Ok(IntakeStep::Continue)
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
}

/// One intake iteration's outcome: `Finish` ends the phase, `Continue` loops again.
enum IntakeStep {
    Finish(IntakePhase),
    Continue,
}

/// Mutable loop state for the intake negotiation.
struct IntakeLoop {
    answers: Vec<IntakeAnswer>,
    rounds: u32,
    coherence_redrafts: u32,
}

/// Immutable inputs for one intake negotiation, bundled so the stage helpers stay small.
struct IntakeCtx<'a> {
    session_id: &'a str,
    goal: &'a GoalSpec,
    ctx: &'a PackContext<'a>,
    settings: &'a IntakeSettings,
    events: &'a Sender<SessionEvent>,
    inputs: &'a mut InputChannel,
    cancel: &'a mut tokio::sync::watch::Receiver<bool>,
}

/// Build intake context: explicit payload.context plus authorized project/workspace so the model
/// does not re-ask for paths the daemon already resolved (dogfood finding #2).
fn build_intake_context(goal: &GoalSpec) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(c) = goal
        .payload
        .get("context")
        .and_then(|v| v.as_str())
        .filter(|c| !c.trim().is_empty())
    {
        parts.push(c.to_string());
    }
    if let Some(project) = goal.payload.get("project").and_then(|v| v.as_str()) {
        parts.push(format!(
            "Authorized coding project name: `{project}`. Do not ask the human for the project \
             name or for a path under this project."
        ));
    }
    if let Some(root) = goal.payload.get("workspace_root").and_then(|v| v.as_str()) {
        parts.push(format!(
            "Authorized workspace_root (absolute, already injected by the daemon): `{root}`. \
             Do not ask for the absolute path to the workspace."
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// The prompt shown for one clarifying question — `affects` included so the human can see *why*
/// it is being asked, not just what.
fn question_prompt(q: &IntakeQuestion) -> String {
    if q.affects.trim().is_empty() {
        q.prompt.clone()
    } else {
        format!("{}\n(affects: {})", q.prompt, q.affects.trim())
    }
}

/// Render a draft contract for human review. This is the freeze UI: what gets built, and — the
/// part that actually matters — what it will be *judged* against.
pub(super) fn render_draft(draft: &GoalContractDraft, rationale: &str) -> String {
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
