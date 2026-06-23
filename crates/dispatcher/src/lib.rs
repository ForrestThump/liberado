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
use liberado_common::{BlockReason, CapabilitySet, DispatchAction, DispatchDecision};
use liberado_provider::{CompletionRequest, Message, Provider, ProviderError, complete_json};
use thiserror::Error;

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

/// A catalog entry: an MCP the dispatcher may route to.
#[derive(Clone, Debug)]
pub struct McpDescriptor {
    pub name: String,
    pub description: String,
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
        }
    }

    /// Route a goal to a guarded [`DispatchDecision`].
    pub async fn dispatch(&self, req: &DispatchRequest) -> Result<DispatchDecision, DispatchError> {
        let goal_hash = goal_hash(&req.goal);

        let classified = self.classify(req).await?;

        let decision =
            match guards::evaluate(&classified, req, &self.tuning, self.max_reaction_depth) {
                Some(reason) => {
                    tracing::info!(
                        goal_hash,
                        classified = %classified.action,
                        confidence = classified.confidence,
                        downgrade = ?reason,
                        "dispatch decision downgraded by guard"
                    );
                    downgrade_to_clarify(classified.confidence, reason)
                }
                None => {
                    tracing::info!(
                        goal_hash,
                        action = %classified.action,
                        confidence = classified.confidence,
                        "dispatch decision"
                    );
                    classified
                }
            };

        Ok(decision)
    }

    /// The classification step: one structured-output inference at temperature 0. Malformed or
    /// empty output is treated like very low confidence and degraded to a safe `Clarify`
    /// (Decision 13 resilience); a real provider failure propagates.
    async fn classify(&self, req: &DispatchRequest) -> Result<DispatchDecision, DispatchError> {
        let request = self.build_request(req);
        match complete_json::<dyn Provider, DispatchDecision>(
            self.provider.as_ref(),
            request,
            decision_schema(),
        )
        .await
        {
            Ok(decision) => Ok(decision),
            // The model responded, but unusably → safe default, don't crash.
            Err(ProviderError::Decode(_)) | Err(ProviderError::EmptyResponse) => {
                tracing::warn!("classification produced unusable output; degrading to Clarify");
                Ok(clarify_fallback())
            }
            // A genuine provider failure — let the caller decide (retry/backoff).
            Err(e) => Err(DispatchError::Provider(e)),
        }
    }

    fn build_request(&self, req: &DispatchRequest) -> CompletionRequest {
        let catalog = req
            .catalog
            .iter()
            .map(|m| format!("- {}: {}", m.name, m.description))
            .collect::<Vec<_>>()
            .join("\n");
        CompletionRequest::new(vec![
            Message::system(SYSTEM_PROMPT),
            Message::user(format!(
                "Goal:\n{}\n\nAvailable MCPs:\n{}",
                req.goal, catalog
            )),
        ])
        .with_temperature(0.0)
    }
}

const SYSTEM_PROMPT: &str = "\
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

Return ONLY JSON of the form:
{\"action\":{\"ExecuteDirect\":{\"seed_calls\":[{\"tool\":\"mcp:tool\",\"args\":{}}]}},\"confidence\":0.9,\"rationale\":\"...\"}
{\"action\":{\"DispatchSubagent\":{\"goal\":\"...\",\"capabilities\":{\"capabilities\":[]},\"allowed_mcps\":[\"...\"],\"success_criteria\":[\"...\"],\"correlation_id\":\"...\"}},\"confidence\":0.8,\"rationale\":\"...\"}
{\"action\":{\"Clarify\":{\"questions\":[\"...\"],\"what_blocked\":\"ambiguous\"}},\"confidence\":0.4,\"rationale\":\"...\"}";

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

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::{Capability, ToolCall};
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
