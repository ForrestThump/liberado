//! # liberado-dispatcher
//!
//! The out-of-band router (Decision 1, `liberado-dispatch-logic-spec.md`). It takes a goal +
//! minimal context and produces a typed [`DispatchDecision`] — `ExecuteDirect`,
//! `DispatchSubagent`, or `Clarify` — via a small pipeline:
//!
//! 1. **classify** — one structured-output inference (temperature 0) turns the goal + MCP catalog
//!    into a candidate decision.
//! 2. **guard** — the deterministic, downgrade-only pipeline ([`guards`]) makes safety emergent:
//!    a misclassification can be wasteful but never unsafe.
//!
//! This slice produces the *decision*; it does not execute it (no MCP clients yet) and does not
//! yet consult procedural memory (the retrieve/record loop, deferred). Those are clean seams.
//!
//! Correctness is engineered, not hoped for: classification is the only probabilistic step, and
//! it is bounded on both sides — malformed output degrades to a safe `Clarify` rather than
//! crashing (Decision 13), and the guards constrain the result regardless of what the model said.

mod guards;

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use liberado_common::config::DispatchTuning;
use liberado_common::{
    BlockReason, CapabilitySet, DispatchAction, DispatchDecision, ProposedAction, ToolCall,
};
use liberado_provider::{CompletionRequest, Message, Provider, ProviderError, complete_json};
use thiserror::Error;
use tracing::Instrument;

// Re-exported so existing `liberado_dispatcher::McpDescriptor` import paths (daemon, eval, this
// crate's own `guards` module) keep working unchanged — it's the same type `liberado_common`'s
// live `CapabilityCatalog` uses now, not a separate one. This crate used to define its own
// `McpDescriptor` (name/description/consequence, no `provenance`), duplicating
// `liberado_common::McpDescriptor` for a dependency-weight reason that no longer applied (this
// crate already depends on `liberado_common` broadly).
pub use liberado_common::McpDescriptor;

/// What the dispatcher needs to route a single goal. The dispatcher deliberately runs with *less*
/// context than the main agent (disjoint partitions); this is everything it sees.
#[derive(Clone, Debug)]
pub struct DispatchRequest {
    /// The restated, self-contained goal.
    pub goal: String,
    /// The MCP catalog the classifier may choose from (names + short descriptions only).
    pub catalog: Vec<McpDescriptor>,
    /// The capabilities currently in force — the ceiling the guards check against (never widened).
    pub capabilities: CapabilitySet,
    /// Correlation-chain depth: 0 for a user-initiated dispatch, >0 for a background reaction.
    pub reaction_depth: u32,
}

/// Errors that abort a dispatch. Malformed model output does **not** appear here — it is handled
/// internally by degrading to a safe `Clarify`. Only a genuine provider failure (transport, rate
/// limit) propagates.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("provider failure during classification: {0}")]
    Provider(ProviderError),
}

/// The out-of-band dispatcher.
pub struct Dispatcher {
    provider: Arc<dyn Provider>,
    tuning: DispatchTuning,
    max_reaction_depth: u32,
    system_prompt: String,
}

impl Dispatcher {
    /// Build a dispatcher over `provider`, with dispatch tunables and the reaction-depth cap
    /// (owned by the concurrency config — passed in so the two configs stay distinct).
    pub fn new(
        provider: Arc<dyn Provider>,
        tuning: DispatchTuning,
        max_reaction_depth: u32,
    ) -> Self {
        Self {
            provider,
            tuning,
            max_reaction_depth,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
        }
    }

    /// Override the system prompt used during classification. Defaults to
    /// [`DEFAULT_SYSTEM_PROMPT`]. The heuristics tuner (`docs/roadmap/heuristics-tuning-engine-plan.md`)
    /// uses this to test candidate prompts against the real dispatch code path without
    /// reimplementing it.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Route a goal to a guarded [`DispatchDecision`].
    pub async fn dispatch(&self, req: &DispatchRequest) -> Result<DispatchDecision, DispatchError> {
        let goal_hash = goal_hash(&req.goal);
        let reaction_depth = req.reaction_depth;

        let span = tracing::info_span!(
            "dispatch",
            goal_hash,
            reaction_depth,
            action = tracing::field::Empty,
            confidence = tracing::field::Empty,
            downgrade = tracing::field::Empty,
        );

        async {
            let mut classified = self.classify(req).await?;
            ensure_correlation(&mut classified, goal_hash);
            enforce_narrow_direct_tools(&mut classified, self.tuning.narrow_direct_tools);

            let decision =
                match guards::evaluate(&classified, req, &self.tuning, self.max_reaction_depth) {
                    Some(reason) => {
                        tracing::Span::current().record("downgrade", &format_args!("{reason:?}"));
                        tracing::info!(
                            classified = %classified.action,
                            confidence = classified.confidence,
                            "dispatch decision downgraded by guard"
                        );
                        downgrade(classified, reason)
                    }
                    None => {
                        tracing::info!(
                            action = %classified.action,
                            confidence = classified.confidence,
                            "dispatch decision"
                        );
                        classified
                    }
                };

            tracing::Span::current().record("action", &format_args!("{}", decision.action.label()));
            tracing::Span::current().record("confidence", &decision.confidence);

            Ok(decision)
        }
        .instrument(span)
        .await
    }

    /// The classification step: one structured-output inference at temperature 0. Malformed or
    /// empty output is treated like very low confidence and degraded to a safe `Clarify`
    /// (Decision 13 resilience); a real provider failure propagates.
    async fn classify(&self, req: &DispatchRequest) -> Result<DispatchDecision, DispatchError> {
        let span = tracing::info_span!("classify", provider = %self.provider.model());

        async {
            let request = self.build_request(req);
            match complete_json::<dyn Provider, DispatchDecision>(
                self.provider.as_ref(),
                request,
                decision_schema(),
            )
            .await
            {
                Ok(decision) => {
                    tracing::debug!(
                        action = %decision.action,
                        confidence = decision.confidence,
                        "classification succeeded"
                    );
                    Ok(decision)
                }
                // The model responded, but unusably → safe default, don't crash.
                Err(ProviderError::Decode(_)) | Err(ProviderError::EmptyResponse) => {
                    tracing::warn!("classification produced unusable output; degrading to Clarify");
                    Ok(clarify_fallback())
                }
                // A genuine provider failure — let the caller decide (retry/backoff).
                Err(e) => {
                    tracing::error!(error = %e, "classification failed");
                    Err(DispatchError::Provider(e))
                }
            }
        }
        .instrument(span)
        .await
    }

    fn build_request(&self, req: &DispatchRequest) -> CompletionRequest {
        let catalog = req
            .catalog
            .iter()
            .map(|m| format!("- {}: {}", m.name, m.description))
            .collect::<Vec<_>>()
            .join("\n");
        CompletionRequest::new(vec![
            Message::system(&self.system_prompt),
            Message::user(format!(
                "Goal:\n{}\n\nAvailable MCPs:\n{}",
                req.goal, catalog
            )),
        ])
        .with_temperature(0.0)
    }
}

/// The default classification system prompt. `pub` so external crates (the heuristics tuner) can
/// diff a candidate prompt against the real baseline.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Liberado's dispatcher: a fast, careful router. Given a goal and a catalog of available \
MCPs (tools), choose exactly ONE action and return it as a single JSON object.

Actions:
- ExecuteDirect: handle a simple, low-consequence goal yourself in a short adaptive tool loop. \
`seed_calls` is just your opening move (the calls you already know you want); leave it empty to \
let the executor decide every step. Use this only when a few steps clearly suffice.
- DispatchSubagent: hand a complex, multi-step, or open-ended goal to a narrowly-scoped subagent.
- Clarify: ask the main agent to resolve ambiguity or a missing parameter before acting.

Bias to safety: when uncertain, or when consequences are high, prefer Clarify or DispatchSubagent \
over ExecuteDirect. Set `confidence` honestly in [0,1].

Two specific cases worth getting right:
- Building or modifying an MCP tool only ever produces a draft PR for human review — that's \
reversible and low-consequence, so route it as ExecuteDirect confidently, but only when a \
code-dispatch (or equivalent tool-building) MCP actually appears in the catalog you were given. A \
goal that talks about building a tool while no such MCP is in your catalog is a capability/tooling \
gap, not something to route around — Clarify instead of assuming one exists. Don't Clarify or \
DispatchSubagent for a real code-dispatch goal unless the goal itself is ambiguous (e.g. which \
existing tool to modify is unclear).
- Open-ended analysis across multiple notes or a range of entries (summarizing, finding recurring \
themes) is complex enough to warrant a DispatchSubagent with its own context slice, even when no \
single step is individually hard.

Return ONLY JSON of the form (use exactly these fields — nothing else):
{\"action\":{\"ExecuteDirect\":{\"seed_calls\":[{\"tool\":\"mcp:tool\",\"args\":{}}],\"relevant_mcps\":[\"...\"]}},\"confidence\":0.9,\"rationale\":\"...\"}
{\"action\":{\"DispatchSubagent\":{\"goal\":\"...\",\"allowed_mcps\":[\"...\"],\"success_criteria\":[\"...\"]}},\"confidence\":0.8,\"rationale\":\"...\"}
{\"action\":{\"Clarify\":{\"questions\":[\"...\"],\"what_blocked\":\"ambiguous\"}},\"confidence\":0.4,\"rationale\":\"...\"}

For ExecuteDirect, also set `relevant_mcps` to the names (from the catalog) of the MCPs this goal \
actually needs — same idea as DispatchSubagent's `allowed_mcps`, just for the direct-execution \
case. Leave it empty if the goal doesn't clearly need any MCP, or if you're unsure (it only \
narrows what the executor sees; it is never the sole source of truth for what's allowed).

For DispatchSubagent, emit only goal, allowed_mcps (names from the catalog), and success_criteria. \
`seed_calls` may be omitted (empty). Do not invent ids or capability objects.";

/// Loose schema for v1 — the prompt carries the shape. A precise JSON Schema (e.g. via `schemars`)
/// is a follow-up that improves real-provider reliability.
fn decision_schema() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

/// The safe default when classification can't be trusted: ask the main agent.
fn clarify_fallback() -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec![
                "I couldn't classify this goal reliably — how should I proceed?".into(),
            ],
            what_blocked: BlockReason::LowConfidence,
        },
        confidence: 0.0,
        rationale: "classification output was unusable".into(),
    }
}

/// Resolve a guard violation to a downgraded decision. A high-consequence block on a *concrete*
/// `ExecuteDirect` (a non-empty seed call list — a known action like "send this email") or on a
/// `DispatchSubagent` (which always carries a restated goal — the classifier's one required field,
/// so there is always something concrete enough to propose) becomes a `Propose`, carrying the
/// action for human approval (Decision 11). Every other case stays a `Clarify` — the most
/// conservative output when there is nothing concrete to propose.
///
/// TODO: the one still-deferred fuzzy case is an empty-seed `ExecuteDirect` (including a bare
/// magnitude-gate hit with no seed calls) — there is no fixed action to propose, since
/// `ExecuteDirect` carries no goal of its own (unlike `DispatchSubagent`); the caller's `goal`
/// isn't threaded into this function today. Approving it would mean "run the adaptive loop on this
/// goal," which needs the same runtime-gated-execution shape `Orchestrator::execute_approved`'s
/// `Subagent` arm now has — likely a `ProposedAction::AdaptiveGoal { goal, relevant_mcps }` run via
/// `Task::new(DIRECT_INSTRUCTIONS, goal)` under a gated runtime, mirroring `Orchestrator::run`'s
/// `ExecuteDirect` arm. Left for a follow-up rather than folded in here.
fn downgrade(classified: DispatchDecision, reason: BlockReason) -> DispatchDecision {
    let confidence = classified.confidence;
    let rationale = classified.rationale;
    if reason == BlockReason::HighConsequence {
        match classified.action {
            DispatchAction::ExecuteDirect { seed_calls, .. } if !seed_calls.is_empty() => {
                return downgrade_to_propose_tool_calls(seed_calls, confidence, rationale);
            }
            DispatchAction::DispatchSubagent {
                goal,
                capabilities,
                allowed_mcps,
                success_criteria,
                ..
            } => {
                return downgrade_to_propose_subagent(
                    goal,
                    capabilities,
                    allowed_mcps,
                    success_criteria,
                    confidence,
                    rationale,
                );
            }
            _ => {}
        }
    }
    downgrade_to_clarify(confidence, reason)
}

/// Build the `Propose` a high-consequence concrete `ExecuteDirect` downgrades to, preserving the
/// original decision's seed calls (as the proposed action), confidence, and rationale. Takes the
/// exact payload rather than the whole `DispatchDecision` — the caller's match arm is what proves
/// this is a concrete `ExecuteDirect`, so there is nothing left to assert (or panic on) in here.
fn downgrade_to_propose_tool_calls(
    seed_calls: Vec<ToolCall>,
    confidence: f32,
    rationale: String,
) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: ProposedAction::ToolCalls(seed_calls),
            rationale: rationale.clone(),
        },
        confidence,
        rationale,
    }
}

/// Build the `Propose` a high-consequence `DispatchSubagent` downgrades to, preserving the goal,
/// narrowed capabilities, MCP scoping, and success criteria the classifier chose —
/// `Orchestrator::execute_approved` dispatches the subagent exactly as scoped here once a human
/// approves it. Takes the exact payload rather than the whole `DispatchDecision`, same reasoning as
/// `downgrade_to_propose_tool_calls` above.
fn downgrade_to_propose_subagent(
    goal: String,
    capabilities: CapabilitySet,
    allowed_mcps: Vec<String>,
    success_criteria: Vec<String>,
    confidence: f32,
    rationale: String,
) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: ProposedAction::Subagent {
                goal,
                capabilities,
                allowed_mcps,
                success_criteria,
            },
            rationale: rationale.clone(),
        },
        confidence,
        rationale,
    }
}

/// Build the conservative `Clarify` a guard downgrade resolves to, preserving the model's
/// confidence for the trace.
fn downgrade_to_clarify(confidence: f32, reason: BlockReason) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec![clarify_question(reason)],
            what_blocked: reason,
        },
        confidence,
        rationale: format!("guard downgrade: {reason:?}"),
    }
}

fn clarify_question(reason: BlockReason) -> String {
    match reason {
        BlockReason::CapabilityGap => {
            "This needs a capability I wasn't granted — should I be given access, or handle it differently?".into()
        }
        BlockReason::HighConsequence => {
            "This is far-reaching or hard to undo (a sweeping change, or something that leaves the system like an email) — should I go ahead?".into()
        }
        BlockReason::DepthLimit => {
            "This reaction chain has gone deep enough that I'm pausing it — should it continue?".into()
        }
        BlockReason::LowConfidence => {
            "I'm not confident enough to act on this — can you confirm the intent?".into()
        }
        BlockReason::Ambiguous => "This goal is ambiguous — which interpretation did you mean?".into(),
        BlockReason::MissingParam => {
            "A required detail is missing — can you provide it?".into()
        }
    }
}

/// A compact, privacy-preserving correlation key for the goal in traces. `DefaultHasher`'s output
/// is not stable across Rust versions — fine for within-run correlation, not cross-version.
fn goal_hash(goal: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    goal.hash(&mut hasher);
    hasher.finish()
}

/// Mint a correlation id for a `DispatchSubagent` whose id the model omitted (it now defaults to
/// empty so terse replies still decode). Derived from the goal hash so retries of the same goal
/// share an id (idempotency + loop-breaking); never the model's to invent.
fn ensure_correlation(decision: &mut DispatchDecision, goal_hash: u64) {
    if let DispatchAction::DispatchSubagent { correlation_id, .. } = &mut decision.action
        && correlation_id.is_empty()
    {
        *correlation_id = format!("sub:{goal_hash:x}");
    }
}

/// Deterministic post-classification enforcement of `DispatchTuning::narrow_direct_tools`: when
/// off, clear whatever `relevant_mcps` the classifier produced so every downstream consumer
/// (`Orchestrator`, `ChatSessions`) sees the same "no narrowing" signal it would if the model had
/// never populated the field — one simple rule everywhere, no separate tunable-awareness needed
/// per consumer. Same pattern as `ensure_correlation`: deterministic code, not model-trusted.
fn enforce_narrow_direct_tools(decision: &mut DispatchDecision, narrow_direct_tools: bool) {
    if !narrow_direct_tools
        && let DispatchAction::ExecuteDirect { relevant_mcps, .. } = &mut decision.action
    {
        relevant_mcps.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::{Capability, Consequence, ToolCall};
    use liberado_provider::{CompletionResponse, MockProvider, ResponseFormat};

    fn scripted(decision: &DispatchDecision) -> Arc<MockProvider> {
        Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text(
                serde_json::to_string(decision).unwrap(),
            )],
        ))
    }

    fn request(capabilities: CapabilitySet, reaction_depth: u32) -> DispatchRequest {
        DispatchRequest {
            goal: "add milk to the shopping list".into(),
            catalog: vec![McpDescriptor {
                name: "tasks-mcp".into(),
                description: "task ops".into(),
                consequence: Consequence::Reversible,
                provenance: None,
            }],
            capabilities,
            reaction_depth,
        }
    }

    fn execute_direct(tool: &str, confidence: f32) -> DispatchDecision {
        DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: vec![ToolCall {
                    tool: tool.into(),
                    args: serde_json::json!({}),
                }],
                relevant_mcps: Vec::new(),
            },
            confidence,
            rationale: "test".into(),
        }
    }

    fn caps(mcp: &str) -> CapabilitySet {
        CapabilitySet::from_iter([Capability::ExecuteMcp(mcp.into())])
    }

    #[tokio::test]
    async fn granted_execute_direct_passes_through_at_temp_zero_json() {
        let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
        let dispatcher = Dispatcher::new(mock.clone(), DispatchTuning::default(), 4);

        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 0))
            .await
            .unwrap();
        assert!(matches!(out.action, DispatchAction::ExecuteDirect { .. }));

        // Classification ran at temperature 0 in structured-output mode.
        let sent = mock.last_request().unwrap();
        assert_eq!(sent.temperature, Some(0.0));
        assert!(matches!(sent.response_format, ResponseFormat::Json { .. }));
    }

    #[tokio::test]
    async fn narrow_direct_tools_default_keeps_relevant_mcps() {
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: vec!["tasks-mcp".into()],
            },
            confidence: 0.95,
            rationale: "test".into(),
        };
        let mock = scripted(&decision);
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 0))
            .await
            .unwrap();
        match out.action {
            DispatchAction::ExecuteDirect { relevant_mcps, .. } => {
                assert_eq!(relevant_mcps, vec!["tasks-mcp".to_string()])
            }
            other => panic!("expected ExecuteDirect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn narrow_direct_tools_off_clears_relevant_mcps() {
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: vec!["tasks-mcp".into()],
            },
            confidence: 0.95,
            rationale: "test".into(),
        };
        let mock = scripted(&decision);
        let tuning = DispatchTuning {
            narrow_direct_tools: false,
            ..DispatchTuning::default()
        };
        let dispatcher = Dispatcher::new(mock, tuning, 4);

        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 0))
            .await
            .unwrap();
        match out.action {
            DispatchAction::ExecuteDirect { relevant_mcps, .. } => {
                assert!(relevant_mcps.is_empty(), "expected relevant_mcps cleared")
            }
            other => panic!("expected ExecuteDirect, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ungranted_mcp_is_downgraded_to_capability_gap() {
        let mock = scripted(&execute_direct("email-mcp:send", 0.95));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 0))
            .await
            .unwrap();
        match out.action {
            DispatchAction::Clarify { what_blocked, .. } => {
                assert_eq!(what_blocked, BlockReason::CapabilityGap)
            }
            other => panic!("expected Clarify(CapabilityGap), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn low_confidence_is_downgraded() {
        let mock = scripted(&execute_direct("tasks-mcp:add", 0.4));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 0))
            .await
            .unwrap();
        match out.action {
            DispatchAction::Clarify { what_blocked, .. } => {
                assert_eq!(what_blocked, BlockReason::LowConfidence)
            }
            other => panic!("expected Clarify(LowConfidence), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn high_consequence_concrete_action_is_downgraded_to_propose() {
        // A granted, confident ExecuteDirect whose seed call hits an External MCP: the consequence
        // gate must turn it into a Propose carrying the call — NOT a Clarify (Decision 11 emit path).
        let request = DispatchRequest {
            goal: "email my boss the update".into(),
            catalog: vec![McpDescriptor {
                name: "email".into(),
                description: "send email".into(),
                consequence: Consequence::External,
                provenance: None,
            }],
            capabilities: caps("email"),
            reaction_depth: 0,
        };
        let mock = scripted(&execute_direct("email:send", 0.95));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher.dispatch(&request).await.unwrap();
        match out.action {
            DispatchAction::Propose {
                proposed_action: liberado_common::ProposedAction::ToolCalls(calls),
                ..
            } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool, "email:send");
            }
            other => panic!("expected Propose(ToolCalls), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn high_consequence_subagent_is_downgraded_to_propose() {
        // A DispatchSubagent whose allowed_mcps touches an External MCP: the consequence gate must
        // turn it into a Propose(Subagent) — NOT a Clarify — since a restated goal is always
        // concrete enough to propose (unlike an empty-seed ExecuteDirect).
        let request = DispatchRequest {
            goal: "summarize this week's reviews and email the boss".into(),
            catalog: vec![McpDescriptor {
                name: "email".into(),
                description: "send email".into(),
                consequence: Consequence::External,
                provenance: None,
            }],
            capabilities: caps("email"),
            reaction_depth: 0,
        };
        let decision = DispatchDecision {
            action: DispatchAction::DispatchSubagent {
                goal: "summarize this week's reviews and email the boss".into(),
                capabilities: CapabilitySet::empty(),
                allowed_mcps: vec!["email".into()],
                success_criteria: vec!["the boss received the summary".into()],
                artifact_target: None,
                model: None,
                correlation_id: "c1".into(),
            },
            confidence: 0.9,
            rationale: "open-ended, touches an external MCP".into(),
        };
        let mock = scripted(&decision);
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher.dispatch(&request).await.unwrap();
        match out.action {
            DispatchAction::Propose {
                proposed_action:
                    liberado_common::ProposedAction::Subagent {
                        goal,
                        allowed_mcps,
                        success_criteria,
                        ..
                    },
                ..
            } => {
                assert_eq!(goal, "summarize this week's reviews and email the boss");
                assert_eq!(allowed_mcps, vec!["email".to_string()]);
                assert_eq!(success_criteria, vec!["the boss received the summary".to_string()]);
            }
            other => panic!("expected Propose(Subagent), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn high_consequence_without_concrete_call_stays_clarify() {
        // A sweeping-destructive goal trips the magnitude gate, but with an *empty* seed list there
        // is no concrete action to propose — so it stays the conservative Clarify.
        let request = DispatchRequest {
            goal: "delete all of my notes".into(),
            catalog: vec![McpDescriptor {
                name: "vault".into(),
                description: "git-tracked vault".into(),
                consequence: Consequence::Reversible,
                provenance: None,
            }],
            capabilities: caps("vault"),
            reaction_depth: 0,
        };
        let decision = DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: Vec::new(),
            },
            confidence: 0.95,
            rationale: "test".into(),
        };
        let mock = scripted(&decision);
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher.dispatch(&request).await.unwrap();
        match out.action {
            DispatchAction::Clarify { what_blocked, .. } => {
                assert_eq!(what_blocked, BlockReason::HighConsequence)
            }
            other => panic!("expected Clarify(HighConsequence), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deep_reaction_is_halted() {
        let mock = scripted(&execute_direct("tasks-mcp:add", 0.95));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        // A confidently-classified, fully-granted call is still halted at the depth cap.
        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 4))
            .await
            .unwrap();
        match out.action {
            DispatchAction::Clarify { what_blocked, .. } => {
                assert_eq!(what_blocked, BlockReason::DepthLimit)
            }
            other => panic!("expected Clarify(DepthLimit), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subagent_passes_through_when_all_mcps_granted() {
        let decision = DispatchDecision {
            action: DispatchAction::DispatchSubagent {
                goal: "review recent decisions".into(),
                capabilities: CapabilitySet::empty(),
                allowed_mcps: vec!["tasks-mcp".into()],
                success_criteria: vec!["a review note exists".into()],
                artifact_target: Some("reviews/".into()),
                model: None,
                correlation_id: "c1".into(),
            },
            confidence: 0.85,
            rationale: "open-ended".into(),
        };
        let mock = scripted(&decision);
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher
            .dispatch(&request(caps("tasks-mcp"), 0))
            .await
            .unwrap();
        assert!(matches!(
            out.action,
            DispatchAction::DispatchSubagent { .. }
        ));
    }

    #[tokio::test]
    async fn malformed_output_degrades_to_clarify() {
        let mock = Arc::new(MockProvider::with_script(
            "mock",
            [CompletionResponse::text("this is not valid json")],
        ));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let out = dispatcher
            .dispatch(&request(CapabilitySet::empty(), 0))
            .await
            .unwrap();
        match out.action {
            DispatchAction::Clarify { what_blocked, .. } => {
                assert_eq!(what_blocked, BlockReason::LowConfidence)
            }
            other => panic!("expected Clarify, got {other:?}"),
        }
        assert_eq!(out.confidence, 0.0);
    }

    #[tokio::test]
    async fn genuine_provider_failure_propagates() {
        // An empty mock yields MockExhausted (a real provider failure, not malformed output).
        let mock = Arc::new(MockProvider::new("mock"));
        let dispatcher = Dispatcher::new(mock, DispatchTuning::default(), 4);

        let err = dispatcher
            .dispatch(&request(CapabilitySet::empty(), 0))
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::Provider(_)));
    }
}
