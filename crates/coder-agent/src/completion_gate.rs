//! The coding pack's adapter onto the kernel completion gate
//! (`liberado_session::completion_gate`, S1 of `docs/roadmap/coding-tui-plan.md`).
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
    Reviewer, ReviewerKind, VerifierStatus, VerifierVerdict,
};
use serde_json::json;
use std::sync::Arc;
use tokio::process::Command;

use crate::CoderProviderFactory;
use crate::critic::parse_critic_verdict;
use crate::roles::{role_instructions, truncate_chars};
use crate::trace::{self, EventLog};

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

    let observer = TraceObserver { events };
    Ok(gate
        .evaluate(&evidence, &keeper, &fresh_refs, &observer)
        .await)
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

/// The real change in the workspace: tracked diff against HEAD plus untracked file names.
///
/// Moved here from `critic.rs` — evidence assembly is a gate concern now. Untracked files are
/// listed by name rather than content because a new-file body can be arbitrarily large and the
/// reviewer's question ("was something added that shouldn't be?") is answerable from the name.
async fn workspace_diff(workspace_root: &str) -> Result<String, CoderError> {
    let tracked = Command::new("git")
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

    let untracked = Command::new("git")
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

#[cfg(test)]
mod tests {
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
