//! Dispatch decision + reporting types (Decision 1, `liberado-dispatch-logic-spec.md`).
//!
//! The dispatcher receives a goal + minimal context and chooses exactly one of four terminal
//! actions. The decision is a **typed, inspectable, loggable, testable artifact** (not free
//! prose) — that is what makes safety engineered rather than hoped-for: deterministic guards
//! run *after* the model over this structure and can only *downgrade* risk.

use serde::{Deserialize, Serialize};

use crate::capability::CapabilitySet;
use crate::model::ModelChoice;

/// The classifier's typed output. Emitted via the provider's structured-output mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchDecision {
    pub action: DispatchAction,
    /// 0.0–1.0 self-reported confidence in the classification.
    pub confidence: f32,
    /// One-line rationale for tracing + procedural-memory recording (never shown to the user).
    pub rationale: String,
}

/// The four terminal actions. `Report` is not here — it is the *return type* of executing
/// `ExecuteDirect`/`DispatchSubagent` (see [`Report`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DispatchAction {
    /// Handle a simple, low-consequence goal in the current context: the executor runs an
    /// **adaptive** tool loop (decide a call, see the result, decide the next) under the
    /// `SMALL_FANOUT` turn budget, then Reports. `seed_calls` is the classifier's optional
    /// *opening move* — the calls it already knows it wants — not a fixed plan; an empty list
    /// means "let the executor decide every step." More than a few steps ⇒ prefer
    /// `DispatchSubagent` instead.
    ExecuteDirect {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        seed_calls: Vec<ToolCall>,
        /// The MCPs the classifier judged relevant to this goal, narrowing what the executor's
        /// runtime surfaces to the model — otherwise every granted MCP's full tool schemas get
        /// sent every turn regardless of relevance (the token-efficiency gap this field closes).
        /// Empty means no narrowing (the full grant applies) — also the effective value when
        /// `DispatchTuning::narrow_direct_tools` is off, since `Dispatcher::dispatch` clears
        /// whatever the model produced here in that case (deterministic, not model-trusted).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        relevant_mcps: Vec<String>,
    },

    /// Hand off to a narrowly-scoped subagent with a disjoint context slice.
    DispatchSubagent {
        /// Restated, self-contained goal. The **only** field the classifier must produce; the rest
        /// default so a terse model reply still decodes (and routes) instead of degrading to a
        /// spurious `Clarify`.
        goal: String,
        /// `base ∩ narrowing` — never widened (Decision 4 invariant). Not produced by the model;
        /// the executor narrows from the request's ceiling + `allowed_mcps`.
        #[serde(default)]
        capabilities: CapabilitySet,
        /// Filtered MCP catalog the subagent may see. Empty = all in-scope MCPs.
        #[serde(default)]
        allowed_mcps: Vec<String>,
        /// How the subagent knows it is done.
        #[serde(default)]
        success_criteria: Vec<String>,
        /// Target zone for any produced artifact (e.g. `"reviews/"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_target: Option<String>,
        /// Model for this subagent; may differ from dispatcher/main.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<ModelChoice>,
        /// Ties writes to this goal (loop-breaking + idempotency). The dispatcher mints this when the
        /// model omits it — it is an internal id, not the model's to invent.
        #[serde(default)]
        correlation_id: String,
    },

    /// Ask the **main agent** (not the user) to resolve before any action is taken.
    Clarify {
        questions: Vec<String>,
        what_blocked: BlockReason,
    },

    /// Emit a [`Proposal`](crate::proposal::Proposal) for human approval instead of acting — the
    /// terminal disposition for a high-consequence action the guards won't auto-run (Decision 11).
    /// This is a post-guard downgrade output, never produced by the classifier, so the guards only
    /// route *into* it and never receive it.
    Propose {
        proposed_action: crate::proposal::ProposedAction,
        rationale: String,
    },
}

impl DispatchAction {
    /// The variant's stable kind-label (no payload), for tracing and metrics. Defined once here
    /// so the dispatcher and daemon can't drift apart when a variant is added.
    pub fn label(&self) -> &'static str {
        match self {
            DispatchAction::ExecuteDirect { .. } => "ExecuteDirect",
            DispatchAction::DispatchSubagent { .. } => "DispatchSubagent",
            DispatchAction::Clarify { .. } => "Clarify",
            DispatchAction::Propose { .. } => "Propose",
        }
    }
}

impl std::fmt::Display for DispatchAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A single tool invocation the classifier proposes: tool name + JSON arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool: String,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// The MCP a tool name belongs to, by the `"<mcp>:<tool>"` convention. A bare name (no colon) is
/// treated as the MCP itself. Used both by the dispatcher's capability guard and the runtime's
/// scope enforcement, so the convention is defined once.
pub fn mcp_of(tool: &str) -> &str {
    tool.split_once(':').map(|(mcp, _)| mcp).unwrap_or(tool)
}

/// Why a [`DispatchAction::Clarify`] was raised. The first two are model-judged; the rest are
/// produced by the deterministic guards (§6), not the classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    Ambiguous,
    MissingParam,
    CapabilityGap,
    /// The action would touch something irreversible or external (consequence guard, §6 #3).
    HighConsequence,
    LowConfidence,
    DepthLimit,
}

/// What flows back to the main agent after Execute/Subagent. The main agent's context never
/// sees tool schemas, raw tool output, or internal dispatch reasoning — only this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub outcome: Outcome,
    /// High-signal, human-readable, short.
    pub summary: String,
    /// Vault paths written (e.g. `"reviews/2026-06-21.md"`).
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Things worth surfacing into ContextPolicy.
    #[serde(default)]
    pub new_high_signal_facts: Vec<String>,
    /// Optional suggested next step for the main agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<String>,
}

/// Terminal status of executed work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    PartiallySucceeded,
    Failed,
    /// Prepared an artifact for human approval rather than acting (Decision 11).
    Proposed,
}

/// Whether a dispatch's *execution* blocks the conversational turn or runs in the background
/// (dispatch spec §10). Classification is always synchronous; only execution varies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecMode {
    /// The turn awaits the Report (user is waiting). May be promoted to `Detach` on timeout.
    #[default]
    Await,
    /// Returns a [`JobHandle`] immediately; the Report is delivered later via vault-mediated
    /// surfacing (the same path hook outputs use).
    Detach,
}

/// Returned immediately for a detached dispatch (and on Await→Detach promotion).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobHandle {
    pub correlation_id: String,
    pub status: JobStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, Zone};

    #[test]
    fn decision_round_trips_as_json() {
        let decision = DispatchDecision {
            action: DispatchAction::DispatchSubagent {
                goal: "Review recent decisions".into(),
                capabilities: CapabilitySet::from_iter([Capability::Read(Zone::vault(
                    "decisions",
                ))]),
                allowed_mcps: vec!["decisions-mcp".into()],
                success_criteria: vec!["A review note exists in reviews/".into()],
                artifact_target: Some("reviews/".into()),
                model: Some(ModelChoice::new("deepseek-chat")),
                correlation_id: "review-2026-06-21".into(),
            },
            confidence: 0.82,
            rationale: "Open-ended, multi-step, produces an artifact".into(),
        };

        let json = serde_json::to_string(&decision).unwrap();
        let back: DispatchDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(decision, back);
    }

    #[test]
    fn exec_mode_defaults_to_await() {
        assert_eq!(ExecMode::default(), ExecMode::Await);
    }
}
