//! Model profiles and role capability floors (Decision 13).
//!
//! Provider-agnostic. The floor is **role-tiered, not a single bar**: the dispatcher's hard
//! requirement is reliable structured output; subagents only need tool-calling. A
//! [`ModelProfile`] declares what a model can do; the config loader assigns models to roles
//! and **fail-fast rejects** any model that doesn't meet its role's [`RequiredCaps`] — this is
//! what keeps the dispatch protocol from breaking when a model is swapped.

use serde::{Deserialize, Serialize};

/// Cost/capability tier of a model, used for per-dispatch selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Capable models for the main agent + dispatcher (the control plane).
    ControlPlane,
    /// Cheaper models for subagents (the work plane), chosen per task complexity.
    WorkPlane,
}

/// How much extended reasoning ("thinking") a role's model should do. Provider-agnostic; the
/// OpenAI-compatible provider maps it to the wire (`reasoning: { effort }`, or `{ enabled: false }`
/// for [`Off`](ReasoningLevel::Off)). Adjustable per role from config so thinking can be dialed down
/// on the cheap glue calls (dispatcher, face) without a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningLevel {
    Off,
    Low,
    Medium,
    High,
}

impl ReasoningLevel {
    /// The wire token for the `reasoning.effort` field (`Off` has no effort — it disables thinking).
    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningLevel::Off => "off",
            ReasoningLevel::Low => "low",
            ReasoningLevel::Medium => "medium",
            ReasoningLevel::High => "high",
        }
    }
}

/// Declared capabilities of a concrete model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Provider-qualified model id, e.g. `"deepseek-chat"`.
    pub name: String,
    /// Native tool/function calling support.
    pub tool_calling: bool,
    /// Reliable structured-output / JSON mode (the dispatcher's hard requirement).
    pub structured_output: bool,
    /// Context window in tokens.
    pub context_window: u32,
    pub tier: ModelTier,
    /// Optional relative cost hint (e.g. USD per Mtok), for per-dispatch selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f32>,
}

/// The minimum capabilities a role demands. The hard floor for *every* role is tool-calling
/// OR structured output; specific roles raise the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredCaps {
    pub tool_calling: bool,
    pub structured_output: bool,
}

/// A role a model can be assigned to in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Conversational orchestration: solid tool-calling + instruction-following.
    MainAgent,
    /// Classification into the typed `DispatchDecision`: requires reliable structured output.
    Dispatcher,
    /// Delegated work: floor is tool-calling.
    Subagent,
}

impl ModelRole {
    /// The capability floor this role enforces.
    pub fn required_caps(self) -> RequiredCaps {
        match self {
            // The dispatcher emits a typed DispatchDecision — structured output is mandatory.
            Self::Dispatcher => RequiredCaps {
                tool_calling: false,
                structured_output: true,
            },
            // The main agent drives tools conversationally.
            Self::MainAgent => RequiredCaps {
                tool_calling: true,
                structured_output: false,
            },
            // Subagents need to call tools.
            Self::Subagent => RequiredCaps {
                tool_calling: true,
                structured_output: false,
            },
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MainAgent => "main_agent",
            Self::Dispatcher => "dispatcher",
            Self::Subagent => "subagent",
        }
    }
}

impl ModelProfile {
    /// Whether this model satisfies a role's capability floor (Decision 13). The config loader
    /// uses this to reject an invalid model→role assignment *before the daemon serves anything*.
    pub fn meets(&self, role: ModelRole) -> bool {
        let req = role.required_caps();
        (!req.tool_calling || self.tool_calling)
            && (!req.structured_output || self.structured_output)
    }
}

/// A model selected for a specific dispatch (carried on `DispatchAction::DispatchSubagent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub name: String,
}

impl ModelChoice {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(tool_calling: bool, structured_output: bool, tier: ModelTier) -> ModelProfile {
        ModelProfile {
            name: "test-model".into(),
            tool_calling,
            structured_output,
            context_window: 32_000,
            tier,
            cost: None,
        }
    }

    #[test]
    fn dispatcher_requires_structured_output() {
        let no_structured = profile(true, false, ModelTier::ControlPlane);
        let structured = profile(true, true, ModelTier::ControlPlane);
        assert!(!no_structured.meets(ModelRole::Dispatcher));
        assert!(structured.meets(ModelRole::Dispatcher));
    }

    #[test]
    fn subagent_only_needs_tool_calling() {
        let p = profile(true, false, ModelTier::WorkPlane);
        assert!(p.meets(ModelRole::Subagent));
        assert!(p.meets(ModelRole::MainAgent));
    }
}
