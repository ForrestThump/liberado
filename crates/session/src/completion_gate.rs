//! The completion gate — **completion is a disputed claim** (S1 of `docs/roadmap/coding-tui-plan.md`).
//!
//! An agent finishing its work and saying "done" is not evidence that it is done. The narrative is
//! written by the same party whose work is in question, and a model that has just spent four
//! attempts on a task is the least neutral reader of its own diff. This module makes the claim
//! adjudicable: a pack may not terminate `Succeeded` until independent reviewers, looking only at
//! *evidence*, agree.
//!
//! # Why this lives in the kernel
//!
//! The gate owns **when it runs and what a verdict means**; the pack owns **evidence assembly**.
//! That split is the whole point — a coding pack assembles a git diff, a vault pack assembles note
//! content, and neither shape is visible here. There is deliberately no git type, no workspace, and
//! no provider in this module: [`GateEvidence`] is strings and deterministic verdicts, and
//! [`Reviewer`] is a trait the pack implements however it likes. The second-domain test for this
//! design is that a "groom this vault note" goal can use the gate unchanged.
//!
//! # The two-layer check
//!
//! Deterministic verifiers run **first** and are not overridable — a model reviewer never rescues a
//! hard test failure. The gate is the *judgment* layer on top of that, and it has two parts:
//!
//! * A **gatekeeper** that persists across attempts. Its prior refutations ride in
//!   [`GateEvidence::prior_refutations`], which is what lets it catch the same defect returning in
//!   new clothes — the failure a fresh reviewer cannot see by construction.
//! * A **fresh quorum** of cold reviewers that see only the evidence, never the argument history.
//!   Approval needs a strict majority.
//!
//! # Fail-closed is the invariant
//!
//! Every ambiguous outcome counts as *refuting*: a reviewer that errors, times out, returns
//! unparseable output, or simply is not there. **A sick reviewer can never lower the bar.** This is
//! the property most worth protecting when editing this file — the natural instinct when a reviewer
//! call fails is to skip it and carry on, and that instinct silently converts the gate into a
//! rubber stamp. The break-check in the tests exists to catch exactly that regression.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Status of one deterministic verifier, mirrored into the kernel so the gate can read verifier
/// outcomes without depending on any pack's verifier types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifierStatus {
    Pass,
    Fail,
    /// The verifier itself broke (crashed, timed out) — distinct from a clean `Fail`, because it
    /// means the check produced no information, not that the work is wrong.
    Error,
}

/// One deterministic verifier result, rendered for a reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierVerdict {
    pub id: String,
    pub status: VerifierStatus,
    pub summary: String,
}

/// What a pack hands the gate. Everything a reviewer is allowed to see, and nothing else — no
/// workspace handle, no provider, no tool access. A reviewer that wants more has to be given it as
/// evidence by the pack, which keeps "what was this judged on" answerable after the fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvidence {
    /// The frozen acceptance criteria, rendered. Reviewers judge against *this*, not against their
    /// own taste — the contract was agreed before implementation started.
    pub contract_summary: String,
    /// Pack-assembled proof: a git diff, an artifact body, whatever the domain's evidence is.
    /// Callers are expected to cap this before it gets here.
    pub artifact_evidence: String,
    /// Deterministic results, already computed. Present so reviewers can see that the hard checks
    /// passed and confine themselves to judgment the machine cannot make.
    pub verifier_verdicts: Vec<VerifierVerdict>,
    /// Bounded history of past rejections — the gatekeeper's memory across attempts.
    pub prior_refutations: Vec<String>,
    /// 1-based attempt number this evidence belongs to.
    pub attempt: u32,
}

impl GateEvidence {
    /// True when every deterministic verifier passed. The gate does not enforce this itself (packs
    /// run verifiers first and stop on a hard fail), but reviewers and tests both want the question
    /// answered in one place.
    pub fn verifiers_all_passed(&self) -> bool {
        self.verifier_verdicts
            .iter()
            .all(|v| v.status == VerifierStatus::Pass)
    }
}

/// One reviewer's judgment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "vote", rename_all = "snake_case")]
pub enum ReviewVote {
    Approve,
    Refute { issues: Vec<String> },
}

impl ReviewVote {
    pub fn is_approve(&self) -> bool {
        matches!(self, ReviewVote::Approve)
    }
}

/// Which role cast a vote — surfaces render the gate's work by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerKind {
    /// Persists across attempts; its refutations become `prior_refutations`. Holds a veto.
    Gatekeeper,
    /// Cold, stateless, sees only the evidence. Votes count toward the quorum.
    Fresh,
    /// Not a voter — proposes one structural change on non-convergence.
    Strategist,
}

impl fmt::Display for ReviewerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReviewerKind::Gatekeeper => write!(f, "gatekeeper"),
            ReviewerKind::Fresh => write!(f, "fresh"),
            ReviewerKind::Strategist => write!(f, "strategist"),
        }
    }
}

/// A vote as it was actually counted, after fail-closed coercion.
///
/// `coerced_from` is the audit trail: when it is `Some`, the reviewer did not return this vote —
/// the gate substituted a refutation because the reviewer failed. Without this field a
/// reviewer outage and a genuine rejection look identical in the logs, and an operator debugging
/// "why did my build stop converging" cannot tell a broken model from a strict one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedVote {
    pub reviewer: String,
    pub kind: ReviewerKind,
    pub vote: ReviewVote,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coerced_from: Option<String>,
}

impl RecordedVote {
    /// Was this vote substituted by the gate rather than returned by the reviewer?
    pub fn was_coerced(&self) -> bool {
        self.coerced_from.is_some()
    }
}

/// The gate's ruling for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum GateVerdict {
    Approved,
    Refuted { issues: Vec<String> },
}

impl GateVerdict {
    pub fn is_approved(&self) -> bool {
        matches!(self, GateVerdict::Approved)
    }
}

/// Everything the gate decided, plus how it got there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOutcome {
    pub verdict: GateVerdict,
    /// Every vote in casting order: gatekeeper first, then the fresh quorum.
    pub votes: Vec<RecordedVote>,
}

impl GateOutcome {
    /// Issues from every refuting vote, deduplicated in first-seen order — this is what becomes the
    /// next attempt's `prior_feedback`.
    pub fn refutation_issues(&self) -> Vec<String> {
        refutation_issues(&self.votes)
    }
}

/// Deduplicated issues across refuting votes. Free function so [`CompletionGate::evaluate`] can
/// build a verdict from votes it has not yet moved into a [`GateOutcome`].
fn refutation_issues(votes: &[RecordedVote]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for vote in votes {
        if let ReviewVote::Refute { issues } = &vote.vote {
            for issue in issues {
                if !out.iter().any(|existing| existing == issue) {
                    out.push(issue.clone());
                }
            }
        }
    }
    out
}

/// Why a reviewer could not produce a vote. Every variant is coerced to a refutation by the gate —
/// this type exists so the *reason* survives into [`RecordedVote::coerced_from`], not so a caller
/// can decide what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum GateError {
    #[error("reviewer call failed: {0}")]
    Backend(String),
    #[error("reviewer output could not be parsed: {0}")]
    Malformed(String),
    #[error("reviewer timed out after {0}s")]
    Timeout(u64),
}

/// One independent review.
///
/// Implementations must **not** decide policy: return `Err` and let the gate coerce it. A reviewer
/// that catches its own error and returns [`ReviewVote::Approve`] defeats the entire mechanism.
#[async_trait]
pub trait Reviewer: Send + Sync {
    /// Stable identifier for logs and events (e.g. `"skeptic-0"`, `"fresh-1"`).
    fn name(&self) -> &str;

    /// Review `evidence`. `fresh` is `true` for cold quorum reviewers and `false` for the
    /// remembered gatekeeper — implementations use it to vary the prompt (a fresh reviewer is not
    /// shown the argument history).
    async fn review(&self, evidence: &GateEvidence, fresh: bool) -> Result<ReviewVote, GateError>;
}

/// Watches votes as they are cast, so a surface can render the gate working rather than waiting for
/// a single verdict at the end. `()` implements this as a no-op for tests and headless callers.
pub trait GateObserver: Send + Sync {
    fn on_vote(&self, vote: &RecordedVote);
}

impl GateObserver for () {
    fn on_vote(&self, _vote: &RecordedVote) {}
}

/// How fresh votes are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quorum {
    /// Strictly more than half of the **configured** fresh reviewers must approve. Ties refute.
    ///
    /// Counting against the configured count rather than the responding count is deliberate: if
    /// one of two reviewers is down, requiring "majority of those who answered" would let a single
    /// reviewer approve alone — an outage would *weaken* the gate exactly when the system is least
    /// healthy.
    StrictMajorityOfFresh,
}

impl Quorum {
    /// Minimum approvals needed out of `configured` reviewers.
    fn approvals_required(&self, configured: u8) -> u32 {
        match self {
            // Strictly more than half: 1 of 1, 2 of 2, 2 of 3, 3 of 4, 3 of 5.
            Quorum::StrictMajorityOfFresh => u32::from(configured) / 2 + 1,
        }
    }
}

/// The gate's configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompletionGate {
    /// How many cold reviewers vote. Default 2.
    pub fresh_reviewers: u8,
    pub quorum: Quorum,
    /// Consecutive refuted attempts before the strategist is consulted. Default 3.
    pub strategist_after: u32,
}

impl Default for CompletionGate {
    fn default() -> Self {
        Self {
            fresh_reviewers: 2,
            quorum: Quorum::StrictMajorityOfFresh,
            strategist_after: 3,
        }
    }
}

impl CompletionGate {
    /// Should the strategist run before the next attempt?
    ///
    /// Takes *consecutive* refutations, not total: a run that alternates refuse/approve/refuse is
    /// making progress and does not need a structural rethink.
    pub fn should_consult_strategist(&self, consecutive_refutations: u32) -> bool {
        self.strategist_after > 0 && consecutive_refutations >= self.strategist_after
    }

    /// Adjudicate one attempt.
    ///
    /// Order is load-bearing. The gatekeeper votes first and its refutation **ends the attempt** —
    /// no fresh reviewer is consulted, because a reviewer that remembers the last three attempts
    /// finding the same defect again is worth more than two cold readers who cannot see the
    /// pattern. Only if it approves does the fresh quorum run.
    ///
    /// Fresh reviewers are consulted sequentially. They are independent and could run concurrently;
    /// that is a latency optimization deliberately deferred, because sequential order makes the
    /// vote stream deterministic and this module's correctness is worth more than a few seconds.
    ///
    /// If `fresh` supplies fewer reviewers than [`CompletionGate::fresh_reviewers`], the missing
    /// ones are recorded as refutations — a reviewer that was never constructed is a reviewer that
    /// did not approve.
    pub async fn evaluate(
        &self,
        evidence: &GateEvidence,
        gatekeeper: &dyn Reviewer,
        fresh: &[&dyn Reviewer],
        observer: &dyn GateObserver,
    ) -> GateOutcome {
        let mut votes: Vec<RecordedVote> = Vec::new();

        // ── 1. Gatekeeper (remembered, holds a veto) ──────────────────────────────────
        let gate_vote = cast(gatekeeper, evidence, false, ReviewerKind::Gatekeeper).await;
        observer.on_vote(&gate_vote);
        let vetoed = !gate_vote.vote.is_approve();
        votes.push(gate_vote);

        if vetoed {
            return GateOutcome {
                verdict: GateVerdict::Refuted {
                    issues: refutation_issues(&votes),
                },
                votes,
            };
        }

        // ── 2. Fresh quorum (cold, evidence only) ─────────────────────────────────────
        let configured = usize::from(self.fresh_reviewers);
        let mut approvals: u32 = 0;
        for slot in 0..configured {
            let vote = match fresh.get(slot) {
                Some(reviewer) => cast(*reviewer, evidence, true, ReviewerKind::Fresh).await,
                None => RecordedVote {
                    reviewer: format!("fresh-{slot}"),
                    kind: ReviewerKind::Fresh,
                    vote: ReviewVote::Refute {
                        issues: vec![
                            "configured fresh reviewer was not supplied — counted as refuting"
                                .to_string(),
                        ],
                    },
                    coerced_from: Some(format!(
                        "no reviewer supplied for configured fresh slot {slot}"
                    )),
                },
            };
            if vote.vote.is_approve() {
                approvals += 1;
            }
            observer.on_vote(&vote);
            votes.push(vote);
        }

        let required = self.quorum.approvals_required(self.fresh_reviewers);
        let verdict = if self.fresh_reviewers > 0 && approvals >= required {
            GateVerdict::Approved
        } else {
            GateVerdict::Refuted {
                issues: refutation_issues(&votes),
            }
        };

        GateOutcome { verdict, votes }
    }
}

/// Run one reviewer and coerce any failure into a refutation. This is the single place fail-closed
/// is implemented; nothing else in this module is allowed to turn an `Err` into a vote.
async fn cast(
    reviewer: &dyn Reviewer,
    evidence: &GateEvidence,
    fresh: bool,
    kind: ReviewerKind,
) -> RecordedVote {
    match reviewer.review(evidence, fresh).await {
        Ok(vote) => RecordedVote {
            reviewer: reviewer.name().to_string(),
            kind,
            vote,
            coerced_from: None,
        },
        Err(e) => RecordedVote {
            reviewer: reviewer.name().to_string(),
            kind,
            vote: ReviewVote::Refute {
                issues: vec![format!("reviewer failed, counted as refuting: {e}")],
            },
            coerced_from: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A reviewer with a scripted answer.
    struct Scripted {
        name: String,
        answer: Result<ReviewVote, GateError>,
    }

    impl Scripted {
        fn approve(name: &str) -> Self {
            Self {
                name: name.to_string(),
                answer: Ok(ReviewVote::Approve),
            }
        }
        fn refute(name: &str, issue: &str) -> Self {
            Self {
                name: name.to_string(),
                answer: Ok(ReviewVote::Refute {
                    issues: vec![issue.to_string()],
                }),
            }
        }
        fn fails(name: &str, e: GateError) -> Self {
            Self {
                name: name.to_string(),
                answer: Err(e),
            }
        }
    }

    #[async_trait]
    impl Reviewer for Scripted {
        fn name(&self) -> &str {
            &self.name
        }
        async fn review(
            &self,
            _evidence: &GateEvidence,
            _fresh: bool,
        ) -> Result<ReviewVote, GateError> {
            self.answer.clone()
        }
    }

    /// Records the `fresh` flag each reviewer was called with.
    struct FreshnessSpy {
        name: String,
        seen: Mutex<Vec<bool>>,
    }

    #[async_trait]
    impl Reviewer for FreshnessSpy {
        fn name(&self) -> &str {
            &self.name
        }
        async fn review(
            &self,
            _evidence: &GateEvidence,
            fresh: bool,
        ) -> Result<ReviewVote, GateError> {
            self.seen.lock().unwrap().push(fresh);
            Ok(ReviewVote::Approve)
        }
    }

    #[derive(Default)]
    struct CollectingObserver {
        votes: Mutex<Vec<RecordedVote>>,
    }

    impl GateObserver for CollectingObserver {
        fn on_vote(&self, vote: &RecordedVote) {
            self.votes.lock().unwrap().push(vote.clone());
        }
    }

    fn evidence() -> GateEvidence {
        GateEvidence {
            contract_summary: "add a --version flag".to_string(),
            artifact_evidence: "diff --git a/src/main.rs".to_string(),
            verifier_verdicts: vec![VerifierVerdict {
                id: "build".to_string(),
                status: VerifierStatus::Pass,
                summary: "cargo build ok".to_string(),
            }],
            prior_refutations: Vec::new(),
            attempt: 1,
        }
    }

    fn gate(fresh_reviewers: u8) -> CompletionGate {
        CompletionGate {
            fresh_reviewers,
            ..CompletionGate::default()
        }
    }

    // ── quorum math ───────────────────────────────────────────────────────────────────

    #[test]
    fn strict_majority_needs_more_than_half() {
        let q = Quorum::StrictMajorityOfFresh;
        assert_eq!(q.approvals_required(1), 1);
        assert_eq!(q.approvals_required(2), 2, "2 reviewers: a tie must refute");
        assert_eq!(q.approvals_required(3), 2);
        assert_eq!(q.approvals_required(4), 3, "4 reviewers: 2-2 must refute");
        assert_eq!(q.approvals_required(5), 3);
    }

    #[tokio::test]
    async fn unanimous_approval_passes_the_gate() {
        let outcome = gate(2)
            .evaluate(
                &evidence(),
                &Scripted::approve("skeptic-0"),
                &[&Scripted::approve("fresh-0"), &Scripted::approve("fresh-1")],
                &(),
            )
            .await;
        assert!(outcome.verdict.is_approved());
        assert_eq!(outcome.votes.len(), 3, "gatekeeper + 2 fresh");
    }

    #[tokio::test]
    async fn a_tie_among_fresh_reviewers_refutes() {
        let outcome = gate(2)
            .evaluate(
                &evidence(),
                &Scripted::approve("skeptic-0"),
                &[
                    &Scripted::approve("fresh-0"),
                    &Scripted::refute("fresh-1", "no test covers the new flag"),
                ],
                &(),
            )
            .await;
        assert_eq!(
            outcome.verdict,
            GateVerdict::Refuted {
                issues: vec!["no test covers the new flag".to_string()]
            },
            "1 of 2 is not a strict majority"
        );
    }

    #[tokio::test]
    async fn majority_of_three_approves() {
        let outcome = gate(3)
            .evaluate(
                &evidence(),
                &Scripted::approve("skeptic-0"),
                &[
                    &Scripted::approve("fresh-0"),
                    &Scripted::refute("fresh-1", "style nit"),
                    &Scripted::approve("fresh-2"),
                ],
                &(),
            )
            .await;
        assert!(outcome.verdict.is_approved(), "2 of 3 is a strict majority");
    }

    // ── the gatekeeper veto ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn gatekeeper_refutation_vetoes_and_skips_the_quorum() {
        let spy = FreshnessSpy {
            name: "fresh-0".to_string(),
            seen: Mutex::new(Vec::new()),
        };
        let outcome = gate(2)
            .evaluate(
                &evidence(),
                &Scripted::refute("skeptic-0", "this is the same defect as attempt 2"),
                &[&spy],
                &(),
            )
            .await;

        assert_eq!(
            outcome.verdict,
            GateVerdict::Refuted {
                issues: vec!["this is the same defect as attempt 2".to_string()]
            }
        );
        assert_eq!(outcome.votes.len(), 1, "the veto ends the attempt");
        assert!(
            spy.seen.lock().unwrap().is_empty(),
            "fresh reviewers must not be consulted after a veto"
        );
    }

    #[tokio::test]
    async fn gatekeeper_is_asked_as_remembered_and_quorum_as_fresh() {
        let keeper = FreshnessSpy {
            name: "skeptic-0".to_string(),
            seen: Mutex::new(Vec::new()),
        };
        let cold = FreshnessSpy {
            name: "fresh-0".to_string(),
            seen: Mutex::new(Vec::new()),
        };
        gate(1).evaluate(&evidence(), &keeper, &[&cold], &()).await;

        assert_eq!(
            *keeper.seen.lock().unwrap(),
            vec![false],
            "the gatekeeper remembers — it must not be asked as fresh"
        );
        assert_eq!(*cold.seen.lock().unwrap(), vec![true]);
    }

    // ── fail-closed ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_failing_fresh_reviewer_counts_as_refuting() {
        for error in [
            GateError::Backend("connection reset".to_string()),
            GateError::Malformed("expected JSON, got prose".to_string()),
            GateError::Timeout(30),
        ] {
            let outcome = gate(2)
                .evaluate(
                    &evidence(),
                    &Scripted::approve("skeptic-0"),
                    &[
                        &Scripted::approve("fresh-0"),
                        &Scripted::fails("fresh-1", error.clone()),
                    ],
                    &(),
                )
                .await;
            assert!(
                !outcome.verdict.is_approved(),
                "a sick reviewer must never lower the bar (error: {error})"
            );
            let coerced = outcome.votes.iter().find(|v| v.was_coerced()).unwrap();
            assert_eq!(
                coerced.coerced_from.as_deref(),
                Some(error.to_string().as_str()),
                "the real failure reason must survive for the operator"
            );
        }
    }

    #[tokio::test]
    async fn a_failing_gatekeeper_vetoes() {
        let outcome = gate(2)
            .evaluate(
                &evidence(),
                &Scripted::fails("skeptic-0", GateError::Timeout(30)),
                &[&Scripted::approve("fresh-0"), &Scripted::approve("fresh-1")],
                &(),
            )
            .await;
        assert!(
            !outcome.verdict.is_approved(),
            "an unreachable gatekeeper must not be treated as consent"
        );
    }

    #[tokio::test]
    async fn missing_reviewers_count_as_refusals_not_as_a_smaller_quorum() {
        // Two configured, only one supplied. If the gate counted a majority of *responders*, this
        // single approval would pass and an outage would quietly weaken the gate.
        let outcome = gate(2)
            .evaluate(
                &evidence(),
                &Scripted::approve("skeptic-0"),
                &[&Scripted::approve("fresh-0")],
                &(),
            )
            .await;
        assert!(!outcome.verdict.is_approved());
        assert_eq!(
            outcome.votes.len(),
            3,
            "the absent reviewer still gets a vote"
        );
        assert!(outcome.votes[2].was_coerced());
    }

    #[tokio::test]
    async fn zero_configured_reviewers_cannot_approve() {
        // Guards a plausible "just disable the gate in config" path from becoming a silent bypass.
        let outcome = gate(0)
            .evaluate(&evidence(), &Scripted::approve("skeptic-0"), &[], &())
            .await;
        assert!(
            !outcome.verdict.is_approved(),
            "a gate with no reviewers approves nothing"
        );
    }

    /// **Break-check** (per the plan's S1 proof): neutering the gate to approve everything must
    /// fail this test. It asserts the one property the whole module exists to provide.
    #[tokio::test]
    async fn break_check_gate_cannot_be_silently_permissive() {
        // Every refusing configuration must produce a non-approval.
        let cases: Vec<(&str, GateOutcome)> = vec![
            (
                "gatekeeper refutes",
                gate(2)
                    .evaluate(
                        &evidence(),
                        &Scripted::refute("skeptic-0", "x"),
                        &[&Scripted::approve("f0"), &Scripted::approve("f1")],
                        &(),
                    )
                    .await,
            ),
            (
                "quorum ties",
                gate(2)
                    .evaluate(
                        &evidence(),
                        &Scripted::approve("skeptic-0"),
                        &[&Scripted::approve("f0"), &Scripted::refute("f1", "x")],
                        &(),
                    )
                    .await,
            ),
            (
                "reviewer errors",
                gate(2)
                    .evaluate(
                        &evidence(),
                        &Scripted::approve("skeptic-0"),
                        &[
                            &Scripted::approve("f0"),
                            &Scripted::fails("f1", GateError::Backend("down".into())),
                        ],
                        &(),
                    )
                    .await,
            ),
        ];
        for (label, outcome) in cases {
            assert!(
                !outcome.verdict.is_approved(),
                "gate approved despite: {label} — the gate has been neutered"
            );
        }
    }

    // ── feedback + observation ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn refutation_issues_are_collected_and_deduplicated() {
        let outcome = gate(2)
            .evaluate(
                &evidence(),
                &Scripted::approve("skeptic-0"),
                &[
                    &Scripted::refute("fresh-0", "no test for the error path"),
                    &Scripted::refute("fresh-1", "no test for the error path"),
                ],
                &(),
            )
            .await;
        assert_eq!(
            outcome.refutation_issues(),
            vec!["no test for the error path".to_string()],
            "the same complaint from two reviewers is one piece of feedback"
        );
    }

    #[tokio::test]
    async fn every_vote_is_observed_in_casting_order() {
        let observer = CollectingObserver::default();
        gate(2)
            .evaluate(
                &evidence(),
                &Scripted::approve("skeptic-0"),
                &[
                    &Scripted::approve("fresh-0"),
                    &Scripted::refute("fresh-1", "x"),
                ],
                &observer,
            )
            .await;

        let seen = observer.votes.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].kind, ReviewerKind::Gatekeeper);
        assert_eq!(seen[1].kind, ReviewerKind::Fresh);
        assert_eq!(seen[2].kind, ReviewerKind::Fresh);
    }

    // ── strategist ────────────────────────────────────────────────────────────────────

    #[test]
    fn strategist_fires_only_after_consecutive_refutations() {
        let g = CompletionGate::default(); // strategist_after = 3
        assert!(!g.should_consult_strategist(0));
        assert!(!g.should_consult_strategist(2));
        assert!(g.should_consult_strategist(3));
        assert!(g.should_consult_strategist(4));
    }

    #[test]
    fn strategist_can_be_disabled_with_zero() {
        let g = CompletionGate {
            strategist_after: 0,
            ..CompletionGate::default()
        };
        assert!(!g.should_consult_strategist(99));
    }

    #[test]
    fn evidence_reports_verifier_health() {
        let mut e = evidence();
        assert!(e.verifiers_all_passed());
        e.verifier_verdicts.push(VerifierVerdict {
            id: "test".to_string(),
            status: VerifierStatus::Fail,
            summary: "2 failing".to_string(),
        });
        assert!(!e.verifiers_all_passed());
    }
}
