//! The coding pack's adapter onto the kernel completion gate
//! (`liberado_session::completion_gate`, S1 of `docs/future-work/coding-tui-plan.md`).
//!
//! Division of labour, and the reason this file is thin: the kernel owns *when the gate runs and
//! what a verdict means* — quorum math, the gatekeeper veto, fail-closed coercion. This module owns
//! only the two things that are genuinely coding-shaped:
//!
//! 1. **Evidence assembly** — turning a workspace into a [`GateEvidence`]: the real git diff, the
//!    frozen contract, and the deterministic verifier results.
//! 2. **A [`Reviewer`] implementation** that asks a model and parses its verdict, reusing
//!    `critic.rs`'s prompt shape and JSON parsing for all three reviewer kinds.
//!
//! Nothing here decides policy. In particular a failed model call returns `Err(GateError)` and lets
//! the kernel coerce it into a refutation — catching the error here and returning
//! `ReviewVote::Approve` would defeat the entire mechanism, which is why [`ModelReviewer::review`]
//! has no fallback path.

use chrono::Utc;
use liberado_coder_core::{
    CoderError, CoderEvent, CoderRoleConfig, CoderRunRequest, CriticVerdict, GateVoteRecord,
    NamedVerdict, VerdictStatus,
};
use liberado_provider::{CompletionRequest, Message, Provider};
use liberado_session::{
    CompletionGate, GateError, GateEvidence, GateObserver, GateOutcome, RecordedVote, ReviewVote,
    Reviewer, ReviewerKind, SessionEvent, SessionEventKind, VerifierStatus, VerifierVerdict,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::live::LIVE_GATE;

use crate::CoderProviderFactory;
use crate::critic::parse_critic_verdict;
use crate::roles::{role_instructions, truncate_chars};
use crate::trace::{self, EventLog};
use crate::workspace_diff;

/// Cap on the diff handed to a reviewer. Matches the legacy critic's budget.
const EVIDENCE_MAX_CHARS: usize = 48_000;

/// Cap on how many prior refutations ride along. The gatekeeper's memory has to be bounded or a
/// long-running goal eventually spends its whole context re-reading its own complaint history.
const PRIOR_REFUTATIONS_MAX: usize = 12;

/// A reviewer backed by one model call.
///
/// `kind` changes only the prompt framing: a gatekeeper is told it has seen this work before and is
/// shown the refutation history; a fresh reviewer is told it is seeing the work cold and gets no
/// history at all. That asymmetry is the point of having both — the gatekeeper catches a defect
/// returning in disguise, the cold readers catch what familiarity has stopped registering.
pub struct ModelReviewer {
    name: String,
    provider: Arc<dyn Provider>,
    role: CoderRoleConfig,
    instructions: String,
}

impl ModelReviewer {
    /// Build a reviewer. `instructions` is the resolved system prompt (from the role's
    /// `prompt`/`prompt_path`), already loaded so `review` stays free of I/O.
    ///
    /// Deliberately does **not** store a [`ReviewerKind`]: whether this reviewer is being asked as
    /// cold or as the remembered gatekeeper is decided by the kernel and arrives as `review`'s
    /// `fresh` argument. Keeping a second copy on the struct would let the two disagree, and the
    /// failure would be silent — a "fresh" reviewer quietly reading the refutation history.
    pub fn new(
        name: impl Into<String>,
        provider: Arc<dyn Provider>,
        role: CoderRoleConfig,
        instructions: String,
    ) -> Self {
        Self {
            name: name.into(),
            provider,
            role,
            instructions,
        }
    }

    /// The user message for one review. Fresh reviewers never see `prior_refutations`.
    fn user_message(&self, evidence: &GateEvidence, fresh: bool) -> String {
        let mut out = String::new();

        if fresh {
            out.push_str(
                "You are reviewing this work COLD. You have not seen earlier attempts and must \
                 judge only what is below, against the acceptance criteria.\n\n",
            );
        } else {
            out.push_str(
                "You are the remembered gatekeeper for this goal. You have reviewed earlier \
                 attempts at the same task. Judge this attempt against the acceptance criteria, \
                 and be especially alert to a defect you previously raised reappearing in a \
                 different form.\n\n",
            );
        }

        out.push_str("Acceptance criteria (frozen before implementation):\n");
        out.push_str(&evidence.contract_summary);
        out.push_str("\n\nAttempt number: ");
        out.push_str(&evidence.attempt.to_string());

        if !evidence.verifier_verdicts.is_empty() {
            out.push_str("\n\nDeterministic checks already run (you cannot override these):\n");
            for v in &evidence.verifier_verdicts {
                out.push_str(&format!(
                    "- {} [{}]: {}\n",
                    v.id,
                    match v.status {
                        VerifierStatus::Pass => "pass",
                        VerifierStatus::Fail => "fail",
                        VerifierStatus::Error => "error",
                    },
                    v.summary
                ));
            }
        }

        if !fresh && !evidence.prior_refutations.is_empty() {
            out.push_str("\n\nYour previous refutations on this goal:\n");
            for issue in &evidence.prior_refutations {
                out.push_str(&format!("- {issue}\n"));
            }
        }

        out.push_str(
            "\n\nEvidence — the actual change (not the author's description of it):\n```\n",
        );
        out.push_str(&evidence.artifact_evidence);
        out.push_str(
            "\n```\n\nRespond with JSON only: {\"quality\":\"acceptable\"} or \
             {\"quality\":\"needs_revision\",\"issues\":[\"...\"]}.",
        );
        out
    }
}

#[async_trait::async_trait]
impl Reviewer for ModelReviewer {
    fn name(&self) -> &str {
        &self.name
    }

    async fn review(&self, evidence: &GateEvidence, fresh: bool) -> Result<ReviewVote, GateError> {
        let mut completion = CompletionRequest::new(vec![
            Message::system(self.instructions.clone()),
            Message::user(self.user_message(evidence, fresh)),
        ]);
        if let Some(temperature) = self.role.temperature {
            completion = completion.with_temperature(temperature);
        }
        if let Some(max_tokens) = self.role.max_tokens {
            completion = completion.with_max_tokens(max_tokens);
        }

        let schema = json!({
            "type": "object",
            "properties": {
                "quality": { "type": "string", "enum": ["acceptable", "needs_revision"] },
                "issues": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["quality"]
        });

        // Every failure below is returned as Err so the KERNEL applies fail-closed. Do not add a
        // fallback that yields a vote here.
        let response = self
            .provider
            .complete(completion.with_json_schema(schema))
            .await
            .map_err(|e| GateError::Backend(e.to_string()))?;
        let content = response
            .content
            .as_deref()
            .ok_or_else(|| GateError::Malformed("reviewer returned empty content".to_string()))?;
        let verdict = parse_critic_verdict(content).map_err(GateError::Malformed)?;

        Ok(match verdict {
            CriticVerdict::Acceptable => ReviewVote::Approve,
            CriticVerdict::NeedsRevision { issues } => ReviewVote::Refute { issues },
        })
    }
}

/// Flatten kernel votes for transport on `CoderRunResult` (the backend has no SessionEvent sender).
pub fn flatten_votes(outcome: &GateOutcome) -> Vec<GateVoteRecord> {
    outcome
        .votes
        .iter()
        .map(|v| GateVoteRecord {
            reviewer: v.reviewer.clone(),
            kind: v.kind.to_string(),
            approved: v.vote.is_approve(),
            issues: match &v.vote {
                ReviewVote::Approve => Vec::new(),
                ReviewVote::Refute { issues } => issues.clone(),
            },
            coerced: v.was_coerced(),
        })
        .collect()
}

/// Kind label used in reviewer names and events.
fn kind_label(kind: ReviewerKind) -> &'static str {
    match kind {
        ReviewerKind::Gatekeeper => "gatekeeper",
        ReviewerKind::Fresh => "fresh",
        ReviewerKind::Strategist => "strategist",
    }
}

/// Map a pack verifier result into the kernel's domain-agnostic shape.
fn verifier_verdict(named: &NamedVerdict) -> VerifierVerdict {
    VerifierVerdict {
        id: named.id.clone(),
        status: match named.verdict.status {
            VerdictStatus::Pass => VerifierStatus::Pass,
            VerdictStatus::Fail => VerifierStatus::Fail,
            VerdictStatus::Error => VerifierStatus::Error,
        },
        summary: named.verdict.summary.clone(),
    }
}

/// Render the frozen acceptance criteria for a reviewer.
fn contract_summary(request: &CoderRunRequest) -> String {
    let criteria = if request.task.success_criteria.is_empty() {
        "(none listed)".to_string()
    } else {
        request
            .task
            .success_criteria
            .iter()
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut out = format!(
        "Task:\n{}\n\nSuccess criteria:\n{criteria}",
        request.task.description
    );
    if let Some(context) = &request.task.context {
        out.push_str("\n\nTask context:\n");
        out.push_str(context);
    }
    out
}

/// Assemble the evidence for one attempt: frozen contract + real diff + verifier results + the
/// gatekeeper's bounded memory.
///
/// This is the coding domain's entire contribution to the gate. A vault pack writes its own version
/// of this function returning note content instead of a diff, and reuses everything else.
pub async fn assemble_evidence(
    request: &CoderRunRequest,
    verifiers: &[NamedVerdict],
) -> Result<GateEvidence, CoderError> {
    let diff = workspace_diff(&request.workspace.root).await?;
    let prior: Vec<String> = request
        .prior_feedback
        .iter()
        .rev()
        .take(PRIOR_REFUTATIONS_MAX)
        .rev()
        .cloned()
        .collect();

    Ok(GateEvidence {
        contract_summary: contract_summary(request),
        artifact_evidence: truncate_chars(&diff, EVIDENCE_MAX_CHARS),
        verifier_verdicts: verifiers.iter().map(verifier_verdict).collect(),
        prior_refutations: prior,
        attempt: request.attempt,
    })
}

/// Forwards each vote into the coder trace as it is cast, so a surface watching the run sees the
/// gate work rather than one verdict at the end.
struct TraceObserver<'a> {
    events: &'a EventLog,
}

impl GateObserver for TraceObserver<'_> {
    fn on_vote(&self, vote: &RecordedVote) {
        trace::push_event(
            self.events,
            CoderEvent::CriticVerdict {
                verdict: match &vote.vote {
                    ReviewVote::Approve => CriticVerdict::Acceptable,
                    ReviewVote::Refute { issues } => CriticVerdict::NeedsRevision {
                        issues: issues.clone(),
                    },
                },
                at: Utc::now(),
            },
        );
        if vote.was_coerced() {
            // Distinguishable in the trace from a reviewer that genuinely rejected the work.
            trace::push_event(
                self.events,
                CoderEvent::LoopGuardTriggered {
                    guard: format!("gate_reviewer_unavailable:{}", vote.reviewer),
                    action: "counted_as_refuting".to_string(),
                    at: Utc::now(),
                },
            );
        }
    }
}

/// Fans out each vote to two observers — the coder trace (for the run log) and the session
/// event bus (for the live frontend, C2).
struct FanoutObserver<'a> {
    a: &'a dyn GateObserver,
    b: &'a dyn GateObserver,
}

impl GateObserver for FanoutObserver<'_> {
    fn on_vote(&self, vote: &RecordedVote) {
        self.a.on_vote(vote);
        self.b.on_vote(vote);
    }
}

/// Streams gate votes to the session event bus as they are cast, so the frontend sees
/// the gate deliberating rather than one verdict at the end (C2).
struct SessionGateObserver {
    session_id: String,
    tx: mpsc::Sender<SessionEvent>,
}

impl GateObserver for SessionGateObserver {
    fn on_vote(&self, vote: &RecordedVote) {
        let event = SessionEvent::new(
            &self.session_id,
            SessionEventKind::CriticVerdict {
                reviewer: vote.reviewer.clone(),
                kind: kind_label(vote.kind).to_string(),
                approved: matches!(vote.vote, ReviewVote::Approve),
                issues: match &vote.vote {
                    ReviewVote::Refute { issues } => issues.clone(),
                    _ => Vec::new(),
                },
                coerced: vote.was_coerced(),
            },
        );
        if let Err(e) = self.tx.try_send(event) {
            tracing::warn!(
                channel_error = ?e,
                "live gate vote dropped — session event channel full or closed"
            );
        }
    }
}

/// Build the reviewers and run the gate for one attempt.
///
/// Reviewer role configs fall back to `[critic]`, so enabling the gate costs no extra config. Every
/// cold reviewer shares one role config but is a separate `ModelReviewer` with its own name — they
/// differ by sampling, not by prompt, which is what makes their agreement meaningful rather than
/// three copies of one opinion.
pub async fn run_gate(
    providers: &dyn CoderProviderFactory,
    request: &CoderRunRequest,
    verifiers: &[NamedVerdict],
    events: &EventLog,
) -> Result<GateOutcome, CoderError> {
    let cfg = &request.config.gate;
    let base = &request.config.critic;

    let gate = CompletionGate {
        fresh_reviewers: cfg.fresh_reviewers,
        quorum: liberado_session::Quorum::StrictMajorityOfFresh,
        strategist_after: cfg.strategist_after,
    };

    let evidence = assemble_evidence(request, verifiers).await?;

    // Gatekeeper.
    let keeper_role = cfg.gatekeeper.clone().unwrap_or_else(|| base.clone());
    let keeper = build_reviewer(
        providers,
        "skeptic-0",
        ReviewerKind::Gatekeeper,
        &keeper_role,
        events,
    )
    .await?;

    // Fresh quorum.
    let fresh_role = cfg.fresh.clone().unwrap_or_else(|| base.clone());
    let mut fresh_owned: Vec<ModelReviewer> = Vec::new();
    for i in 0..cfg.fresh_reviewers {
        fresh_owned.push(
            build_reviewer(
                providers,
                format!("fresh-{i}"),
                ReviewerKind::Fresh,
                &fresh_role,
                events,
            )
            .await?,
        );
    }
    let fresh_refs: Vec<&dyn Reviewer> = fresh_owned.iter().map(|r| r as &dyn Reviewer).collect();

    let trace_obs = TraceObserver { events };
    if let Ok(chan) = LIVE_GATE.try_with(|(tx, id)| (tx.clone(), id.clone())) {
        let session_obs = SessionGateObserver {
            session_id: chan.1,
            tx: chan.0,
        };
        let fanout = FanoutObserver {
            a: &trace_obs,
            b: &session_obs,
        };
        Ok(gate
            .evaluate(&evidence, &keeper, &fresh_refs, &fanout)
            .await)
    } else {
        Ok(gate
            .evaluate(&evidence, &keeper, &fresh_refs, &trace_obs)
            .await)
    }
}

async fn build_reviewer(
    providers: &dyn CoderProviderFactory,
    name: impl Into<String>,
    kind: ReviewerKind,
    role: &CoderRoleConfig,
    events: &EventLog,
) -> Result<ModelReviewer, CoderError> {
    let name = name.into();
    trace::push_event(
        events,
        CoderEvent::RoleStarted {
            role: format!("{}:{name}", kind_label(kind)),
            model: role.model.clone(),
            at: Utc::now(),
        },
    );
    let provider = providers.provider_for("critic", role)?;
    let instructions = role_instructions(role, "critic").await?;
    Ok(ModelReviewer::new(
        name,
        provider,
        role.clone(),
        instructions,
    ))
}

/// The strategist's system prompt. Deliberately narrow: it may change the *approach*, never the
/// bar. A role that can rewrite acceptance criteria when the work is hard is not a strategist, it
/// is a way to lose.
const STRATEGIST_SYSTEM_PROMPT: &str = "\
You are a strategist on a stuck software goal. Several implementation attempts have been refused by \
independent reviewers for substantially the same reasons, which means the approach — not the effort \
— is wrong.

Read the goal, its acceptance criteria, and the rejection history. Propose EXACTLY ONE structural \
change to how the work is being approached: a different decomposition, a different place to make \
the change, a dependency or abstraction to introduce or remove, an ordering change.

Hard rules:
- You may NOT weaken, reinterpret, or drop any acceptance criterion. They are frozen. If the \
criteria seem impossible, say so plainly and explain why — do not quietly lower the bar.
- Propose ONE change, not a list. The value here is focus.
- Be concrete and actionable. \"Refactor for clarity\" is useless; \"move the retry logic out of \
the request builder into the caller so the timeout is testable\" is useful.
- Do not write the implementation. Describe the change and why the previous approach kept failing.

Answer in under 200 words, as plain prose. No preamble, no JSON.";

/// Consult the strategist after repeated refutations. Returns the directive to inject into the next
/// attempt, or `None` when it produced nothing usable.
///
/// **Best-effort by contract.** Every failure path returns `Ok(None)` rather than an error: this
/// runs after work that already exists, and a strategist outage must not destroy a run that is
/// merely struggling. The next attempt simply proceeds without a directive, exactly as it would
/// have before this role existed.
pub async fn run_strategist(
    providers: &dyn CoderProviderFactory,
    request: &CoderRunRequest,
    refutation_history: &[String],
) -> Result<Option<String>, CoderError> {
    let cfg = &request.config.gate;
    let role = cfg
        .strategist
        .clone()
        .unwrap_or_else(|| request.config.critic.clone());

    let provider = match providers.provider_for("critic", &role) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "strategist provider unavailable — continuing without a directive");
            return Ok(None);
        }
    };

    let history = if refutation_history.is_empty() {
        "(none recorded)".to_string()
    } else {
        refutation_history
            .iter()
            .rev()
            .take(PRIOR_REFUTATIONS_MAX)
            .rev()
            .map(|r| format!("- {r}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let user = format!(
        "{}\n\nAttempts so far: {}\n\nWhy each attempt was refused:\n{history}",
        contract_summary(request),
        request.attempt.saturating_add(1),
    );

    let mut completion = CompletionRequest::new(vec![
        Message::system(STRATEGIST_SYSTEM_PROMPT),
        Message::user(user),
    ]);
    if let Some(max_tokens) = role.max_tokens {
        completion = completion.with_max_tokens(max_tokens);
    }

    match provider.complete(completion).await {
        Ok(response) => {
            let directive = response
                .content
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty());
            match &directive {
                Some(d) => tracing::info!(
                    attempt = request.attempt,
                    directive = %truncate_chars(d, 200),
                    "strategist proposed a structural change"
                ),
                None => {
                    tracing::warn!("strategist returned empty — continuing without a directive")
                }
            }
            Ok(directive)
        }
        Err(e) => {
            tracing::warn!(error = %e, "strategist call failed — continuing without a directive");
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    /// C2's actual wiring: a vote must land on the session bus as a `CriticVerdict`, carrying the
    /// reviewer, whether it approved, and any refuting issues.
    ///
    /// Scope (R5): this covers the observer — the piece the branch adds — not the gate's voting
    /// logic, which the existing gate tests own. The observer is where a wrong event kind or a
    /// dropped field would land, and nothing else asserts it.
    #[tokio::test]
    async fn a_vote_reaches_the_session_bus_as_a_critic_verdict() {
        use liberado_session::{ReviewVote, ReviewerKind, SessionEventKind};

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let obs = super::SessionGateObserver {
            session_id: "s-1".into(),
            tx,
        };

        obs.on_vote(&super::RecordedVote {
            reviewer: "critic-a".into(),
            kind: ReviewerKind::Fresh,
            vote: ReviewVote::Refute {
                issues: vec!["tests do not cover the new branch".into()],
            },
            coerced_from: None,
        });

        let ev = rx.try_recv().expect("the vote must reach the bus");
        match ev.kind {
            SessionEventKind::CriticVerdict {
                reviewer,
                approved,
                issues,
                ..
            } => {
                assert_eq!(reviewer, "critic-a");
                assert!(!approved, "a Refute must not read as approved");
                assert_eq!(
                    issues,
                    vec!["tests do not cover the new branch".to_string()]
                );
            }
            other => panic!("expected CriticVerdict, got {other:?}"),
        }
    }

    use super::*;
    use liberado_coder_core::{Verdict, VerdictStatus};

    fn named(id: &str, status: VerdictStatus, summary: &str) -> NamedVerdict {
        NamedVerdict {
            id: id.to_string(),
            kind: "command".to_string(),
            verdict: Verdict {
                status,
                signature: None,
                summary: summary.to_string(),
                findings: Vec::new(),
                log_excerpt: None,
            },
        }
    }

    fn evidence_with(prior: Vec<String>) -> GateEvidence {
        GateEvidence {
            contract_summary: "Task:\nadd --version\n\nSuccess criteria:\n- prints a semver"
                .to_string(),
            artifact_evidence: "diff --git a/src/main.rs b/src/main.rs".to_string(),
            verifier_verdicts: vec![VerifierVerdict {
                id: "build".to_string(),
                status: VerifierStatus::Pass,
                summary: "ok".to_string(),
            }],
            prior_refutations: prior,
            attempt: 3,
        }
    }

    fn reviewer() -> ModelReviewer {
        // The provider is never called in prompt-shape tests.
        ModelReviewer {
            name: "r".to_string(),
            provider: Arc::new(liberado_provider::MockProvider::new("mock")),
            role: CoderRoleConfig {
                model: "m".to_string(),
                prompt_path: None,
                prompt: None,
                temperature: None,
                max_tokens: None,
                max_turns: None,
                reasoning: None,
            },
            instructions: "you are a reviewer".to_string(),
        }
    }

    #[test]
    fn fresh_reviewers_never_see_the_refutation_history() {
        let e = evidence_with(vec!["you forgot the error path again".to_string()]);
        let prompt = reviewer().user_message(&e, true);

        assert!(
            !prompt.contains("you forgot the error path again"),
            "a cold reviewer that can read the argument history is not cold"
        );
        assert!(prompt.contains("COLD"));
        assert!(prompt.contains("add --version"), "criteria must be present");
    }

    #[test]
    fn the_gatekeeper_is_shown_its_own_prior_refutations() {
        let e = evidence_with(vec!["you forgot the error path again".to_string()]);
        let prompt = reviewer().user_message(&e, false);

        assert!(prompt.contains("you forgot the error path again"));
        assert!(
            prompt.contains("different form"),
            "the gatekeeper must be told what it is looking for"
        );
    }

    #[test]
    fn verifier_results_are_shown_as_non_overridable() {
        let prompt = reviewer().user_message(&evidence_with(Vec::new()), true);
        assert!(prompt.contains("cannot override"));
        assert!(prompt.contains("build [pass]: ok"));
    }

    #[test]
    fn pack_verdicts_map_onto_kernel_statuses() {
        assert_eq!(
            verifier_verdict(&named("t", VerdictStatus::Pass, "ok")).status,
            VerifierStatus::Pass
        );
        assert_eq!(
            verifier_verdict(&named("t", VerdictStatus::Fail, "2 failing")).status,
            VerifierStatus::Fail
        );
        assert_eq!(
            verifier_verdict(&named("t", VerdictStatus::Error, "crashed")).status,
            VerifierStatus::Error,
            "an Error verifier must not be flattened into Fail — it produced no information"
        );
    }

    #[test]
    fn prior_refutations_are_bounded_to_the_most_recent() {
        let all: Vec<String> = (0..30).map(|i| format!("issue-{i}")).collect();
        let kept: Vec<String> = all
            .iter()
            .rev()
            .take(PRIOR_REFUTATIONS_MAX)
            .rev()
            .cloned()
            .collect();

        assert_eq!(kept.len(), PRIOR_REFUTATIONS_MAX);
        assert_eq!(
            kept.last().unwrap(),
            "issue-29",
            "the most recent complaint must survive truncation"
        );
        assert_eq!(kept.first().unwrap(), "issue-18");
    }
}
