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
        /// Optional explicit capability narrowing (`base ∩ this` — Decision 4). Not produced by
        /// the model (defaults empty). When empty, the orchestrator derives the risk-gate set from
        /// the dispatch ceiling ∩ `allowed_mcps` so the gate matches the scoped tool catalog.
        #[serde(default)]
        capabilities: CapabilitySet,
        /// Filtered MCP catalog the subagent may see (and, when `capabilities` is empty, the MCP
        /// names used to derive the risk-gate set). Empty = all in-scope MCPs under the ceiling.
        #[serde(default)]
        allowed_mcps: Vec<String>,
        /// How the subagent knows it is done.
        #[serde(default)]
        success_criteria: Vec<String>,
        /// Target zone for any produced artifact (e.g. `"reviews/"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_target: Option<String>,
        /// Where this subagent's terminal [`Report`] should go — see [`Delivery`]. Defaults to
        /// [`Delivery::Summarize`] (the pre-existing behaviour), so an omitted field is the safe
        /// value and every persisted decision round-trips unchanged.
        #[serde(default, skip_serializing_if = "Delivery::is_summarize")]
        delivery: Delivery,
        /// How much room this subagent needs — see [`Depth`]. Defaults to [`Depth::Normal`], so an
        /// omitted field is the safe value and persisted decisions round-trip unchanged.
        #[serde(default, skip_serializing_if = "Depth::is_normal")]
        depth: Depth,
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

/// How much room a subagent gets to work — its turn budget, declared rather than inferred.
///
/// # Why this is not derived
///
/// It was, and the derivation was wrong in a way that took a live failure to see. "Research" was
/// inferred from *every allowed MCP being `ReadOnly`*, and that one predicate silently set three
/// unrelated things: the turn budget, the loop profile, and whether a report could bypass the main
/// agent. They correlated on web research, which is why it looked clean.
///
/// Then a deep-research goal mentioned the vault. The classifier reasonably included the vault MCP,
/// which is `Reversible`, so the dispatch was "not read-only", so it got 8 turns instead of 30 and
/// no wrap-up reserve — and failed at the budget with nothing to show. The task's *depth* has
/// nothing to do with which MCPs it happens to touch, and inferring one from the other means a goal
/// gets a smaller budget for mentioning where the answer should go.
///
/// Declaring it also puts the knob where the knowledge is. Any dispatch source may set it: the
/// human, the main agent relaying them, or an orchestration agent that discovers mid-run that some
/// sub-question is load-bearing in a way nobody anticipated when the goal was written. It is capped
/// by the pool's configured ceiling, so raising it is a request within an envelope the operator set,
/// never an escalation past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    /// A lookup or a single hop. Fewer turns than the default.
    Shallow,
    /// The default working budget for a scoped subagent.
    #[default]
    Normal,
    /// Open-ended gathering — deep research, multi-source synthesis, review across many notes.
    /// Turn-hungry in a way acting work is not, and the case the wrap-up reserve exists for.
    Deep,
}

impl Depth {
    /// `true` for the default, so serde can omit it and leave persisted decisions byte-identical.
    pub fn is_normal(&self) -> bool {
        matches!(self, Depth::Normal)
    }

    /// Stable kind-label for tracing (mirrors [`DispatchAction::label`]).
    pub fn label(&self) -> &'static str {
        match self {
            Depth::Shallow => "shallow",
            Depth::Normal => "normal",
            Depth::Deep => "deep",
        }
    }
}

/// Where a subagent's terminal [`Report`] goes.
///
/// # Why this is a routing decision and not a formatting one
///
/// A subagent's findings reach the human by being *restated* by the main agent: the orchestrator
/// hands back a `Report`, the face agent ingests the body and writes its own prose version. For an
/// action ("book the appointment") that is exactly right — the face agent is the one that can
/// re-dispatch, ask a follow-up question, or explain what went wrong. For a **retrieval** ("what
/// does the vault say about X", "research Y") it is pure loss: the body is paid for twice (once
/// ingested, once re-emitted) and the re-emission is lossy, because a summary of a summary drops
/// detail the human asked for. Decoding is sequential, so that second pass is also the slow one.
///
/// So delivery is chosen **per dispatch**, and the choice is guarded, not trusted:
///
/// * Only a dispatch that can exclusively *read* may use a non-[`Summarize`](Self::Summarize) sink.
///   If something happened out in the world, the main agent narrates it — full stop. That guard is
///   a checkable property (every allowed MCP is [`Consequence::ReadOnly`](crate::Consequence)), not
///   a judgement call about what "kind" of task this is.
/// * A run that did not cleanly succeed falls back to `Summarize` regardless of what was asked for,
///   because a failed or half-finished run is precisely when the main agent needs to see the detail
///   in order to react to it.
///
/// Those two rules are what make an imperfect choice cheap. Mis-routing a *successful* retrieval
/// only means the human reads it unfiltered; mis-routing a *failure* is caught by construction.
///
/// This governs the terminal report only. Mid-run clarification is a separate channel (the
/// `AskHuman` capability) and is unaffected.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Delivery {
    /// Return the `Report` to the main agent, which reads it and narrates the result. The default,
    /// and the only delivery available to a dispatch that can act on the world.
    #[default]
    Summarize,
    /// Write the report body to `path` in the vault, and return only a **receipt** to the main
    /// agent (what was written, and where). The write is performed by the orchestrator as a single
    /// deterministic tool call — no model reads the body on the way there.
    ///
    /// `path` is zone-qualified and relative to the vault root (e.g. `"research/2026-07-25-x.md"`).
    /// A bare filename names no zone and is rejected, the same way a path-addressed write with no
    /// resolvable zone is (see [`write_target`](crate::catalog::write_target)).
    Vault { path: String },
}

impl Delivery {
    /// `true` for the default sink. Used as serde's `skip_serializing_if` so an unset delivery is
    /// absent from the JSON entirely, keeping already-persisted decisions byte-identical.
    pub fn is_summarize(&self) -> bool {
        matches!(self, Delivery::Summarize)
    }

    /// The variant's stable kind-label, for tracing and metrics (mirrors
    /// [`DispatchAction::label`], and for the same reason: one definition, no drift).
    pub fn label(&self) -> &'static str {
        match self {
            Delivery::Summarize => "summarize",
            Delivery::Vault { .. } => "vault",
        }
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

/// The bare tool name (without the `"<mcp>:"` prefix), the companion to [`mcp_of`] for the same
/// `"<mcp>:<tool>"` convention — a bare name (no colon) is its own bare tool name too, mirroring
/// `mcp_of`'s fallback. Used by the zone-write-class guard (§6 #2) to look up a tool's declared
/// zone within its owning MCP's descriptor.
pub fn bare_tool_name(tool: &str) -> &str {
    tool.split_once(':').map(|(_, bare)| bare).unwrap_or(tool)
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
    /// The action would write to a zone whose declared `WriteClass` doesn't allow a direct agent
    /// write (`ProposalOnly`/`HumanOnly`) — the zone-write-class guard (§6 #2). Distinct from
    /// `HighConsequence`: this gates on *where* a write lands, not how risky the MCP's effects are
    /// in general.
    ZoneRestricted,
    LowConfidence,
    DepthLimit,
    /// Classification output could not be decoded at all — malformed JSON, or an empty response.
    ///
    /// Split out from [`LowConfidence`](Self::LowConfidence), which it used to share. The two look
    /// identical downstream and want opposite treatment: low confidence means the model understood
    /// the goal and is unsure, which is a *goal* problem; unusable output means the model did not
    /// answer in the required shape at all, which is a transient provider problem worth one retry.
    /// A failed cron could not be told apart from an ambiguous one without re-running the classifier
    /// offline (homelab evening-debrief, 2026-07-26).
    UnusableOutput,
    /// The action asks a human something, and this actor holds no
    /// [`AskHuman`](crate::Capability::AskHuman) capability — a cron, a webhook reaction, an
    /// unattended profile.
    ///
    /// `Clarify` presupposes an interlocutor. For an unattended dispatch there is none, so the
    /// question is not a conservative fallback but a dead end: it is delivered to nobody and the run
    /// is spent. The capability already declared this ("structurally unable to block on a person who
    /// isn't there") and the dispatcher simply never consulted it.
    Unattended,
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
    /// Set by the runtime, not the model: this run raised a proposal / permission-request that was
    /// **already surfaced to the human out-of-band** (an interactive notification was sent). It is
    /// the signal a chat surface uses to suppress a redundant "you need to grant permission" reply —
    /// the out-of-band notification is the sole, non-duplicated communication. Left `false` for an
    /// ordinary report; serialized only when true so existing persisted reports round-trip unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred_to_human: bool,
    /// How many tool calls during this run were byte-exact repeats of an earlier one (same tool name,
    /// same serialised arguments). Set by the executor, not the model, and serialised only when > 0
    /// so existing persisted reports round-trip unchanged.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeat_calls: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Report {
    pub fn with_repeat_calls(mut self, count: usize) -> Self {
        self.repeat_calls = count;
        self
    }
}

/// Terminal status of one **execution** — what an executor's `Report` says it achieved.
///
/// **Not the same thing as `SessionStatus`, and deliberately not merged with it** (V1, 2026-07-14).
/// This is a level below: one execution inside a session. `PartiallySucceeded` and `Proposed` have no
/// meaning as *session* states — a session does not sit in "proposed" — and folding them together
/// would either lose those two variants or pollute every surface's status rendering with states it
/// cannot act on.
///
/// The one conversion that matters (`Outcome` → `TerminalKind`, when an execution *is* the whole
/// session) lives in exactly one place: `Disposition::terminal_summary`. Keep it that way — an
/// inline second copy of that mapping is how a status starts lying.
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
                delivery: Delivery::Vault {
                    path: "reviews/2026-06-21.md".into(),
                },
                depth: Depth::Deep,
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

    #[test]
    fn depth_is_normal_true_for_normal() {
        assert!(Depth::Normal.is_normal());
        assert!(!Depth::Shallow.is_normal());
        assert!(!Depth::Deep.is_normal());
    }

    #[test]
    fn delivery_is_summarize_true_for_summarize() {
        assert!(Delivery::Summarize.is_summarize());
        assert!(
            !Delivery::Vault {
                path: "x.md".into()
            }
            .is_summarize()
        );
    }

    #[test]
    fn dispatch_action_display_matches_label() {
        let ed = DispatchAction::ExecuteDirect {
            seed_calls: vec![],
            relevant_mcps: vec![],
        };
        let ds = DispatchAction::DispatchSubagent {
            goal: "x".into(),
            capabilities: CapabilitySet::empty(),
            allowed_mcps: vec![],
            success_criteria: vec![],
            artifact_target: None,
            model: None,
            correlation_id: "".into(),
            delivery: Delivery::Summarize,
            depth: Depth::Normal,
        };
        let cl = DispatchAction::Clarify {
            questions: vec!["?".into()],
            what_blocked: crate::BlockReason::Ambiguous,
        };
        assert_eq!(ed.to_string(), "ExecuteDirect");
        assert_eq!(ds.to_string(), "DispatchSubagent");
        assert_eq!(cl.to_string(), "Clarify");
    }

    #[test]
    fn bare_tool_name_strips_prefix() {
        assert_eq!(bare_tool_name("mcp:tool"), "tool");
        assert_eq!(bare_tool_name("bare_tool"), "bare_tool");
        assert_eq!(bare_tool_name("a:b:c"), "b:c");
    }
}

/// Property tests for the `"<mcp>:<tool>"` name convention and the grant semantics built on it:
/// split/rejoin round-trips, `ExecuteMcp` authorizing every tool on its server, and `ExecuteTool`
/// authorizing exactly one tool and nothing else on that server.
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;

    use super::*;
    use crate::capability::{Capability, CapabilitySet};

    // ── Strategies ────────────────────────────────────────────────────────────────────────────

    /// Tool names 0-30 chars, with fixed seeds guaranteeing each of the three shapes the
    /// convention admits: no colon (a bare name), a single colon (`mcp:tool`), and multiple
    /// colons (`a:b:c`, where `bare_tool_name` keeps the `b:c` tail).
    fn name_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            10 => "[a-zA-Z0-9:]{0,30}",
            1 => Just("plain_tool_name".to_string()),
            1 => Just("mcp:tool".to_string()),
            1 => Just("a:b:c".to_string()),
        ]
    }

    /// MCP server names: 0-20 chars, never containing `:` — a server name with a `:` could not be
    /// recovered by `mcp_of` from a qualified `"<mcp>:<tool>"` name, so such a name is outside the
    /// convention the grants are defined over.
    fn mcp_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_-]{0,20}"
    }

    /// Tool names: 0-20 chars. Colons are allowed — the convention treats everything after the
    /// first `:` as the bare tool name, so a tool may itself contain colons.
    fn tool_strategy() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9:]{0,20}"
    }

    // ── Properties ────────────────────────────────────────────────────────────────────────────

    /// Splitting a qualified `"<mcp>:<tool>"` name at the first `:` and rejoining reproduces it
    /// exactly: `mcp_of` yields the prefix and `bare_tool_name` the remainder (including any
    /// further colons, so `a:b:c` round-trips). A bare name (no `:`) has no split/rejoin — both
    /// helpers fall back to the whole string, so the assertion there is that each half equals the
    /// name itself rather than reconstructing to the non-identity `"name:name"`.
    fn tool_name_reconstruction(name: String) -> bool {
        if name.contains(':') {
            format!("{}:{}", mcp_of(&name), bare_tool_name(&name)) == name
        } else {
            mcp_of(&name) == name.as_str() && bare_tool_name(&name) == name.as_str()
        }
    }

    /// `ExecuteMcp(m)` authorizes every `"m:<tool>"` on that server: `grants_tool` resolves the
    /// MCP from the qualified name's prefix and matches it against the server grant, so the tool
    /// name never needs to be known at grant time.
    fn mcp_grant_authorizes_all_tools(mcp: String, tool: String) -> bool {
        let caps = CapabilitySet::from_iter([Capability::ExecuteMcp(mcp.clone())]);
        let full = format!("{}:{}", mcp, tool);
        caps.grants_tool(&full)
    }

    /// `ExecuteTool("m:tool")` authorizes exactly that tool: the qualified name itself, and no
    /// other tool on the same server unless it is the same tool. This is the property that keeps
    /// a per-tool grant from leaking into the rest of a server's catalog.
    fn tool_grant_is_specific(mcp: String, tool: String, other: String) -> bool {
        let caps = CapabilitySet::from_iter([Capability::ExecuteTool(format!("{}:{}", mcp, tool))]);
        let full = format!("{}:{}", mcp, tool);
        let diff = format!("{}:{}", mcp, other);
        caps.grants_tool(&full) && (tool == other || !caps.grants_tool(&diff))
    }

    proptest! {
        #[test]
        fn prop_tool_name_reconstruction(name in name_strategy()) {
            prop_assert!(tool_name_reconstruction(name));
        }

        #[test]
        fn prop_mcp_grant_authorizes_all_tools(mcp in mcp_strategy(), tool in tool_strategy()) {
            prop_assert!(mcp_grant_authorizes_all_tools(mcp, tool));
        }

        #[test]
        fn prop_tool_grant_is_specific(
            mcp in mcp_strategy(),
            tool in tool_strategy(),
            other in tool_strategy(),
        ) {
            prop_assert!(tool_grant_is_specific(mcp, tool, other));
        }
    }
}
