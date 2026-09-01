//! Builder steps kept off `sessions.rs` so that file stays at its function/ploc baseline.

use std::sync::Arc;

use liberado_common::RiskWaiverSet;
use liberado_session::GoalSessionHub;

use super::ChatSessions;
use crate::{DEFAULT_SYSTEM_PROMPT, HUMAN_INTERFACE_SYSTEM_PROMPT};

impl ChatSessions {
    /// Set the risk-waiver set passed through to the dispatcher's pre-flight magnitude guard
    /// and every runtime gate. Empty by default — the heuristic fires as before.
    pub fn with_risk_waivers(mut self, waivers: RiskWaiverSet) -> Self {
        self.risk_waivers = waivers;
        self
    }

    /// Override the system prompt written as the root node of new conversations.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Attach the goal session hub so `delegate` and non-`ExecuteDirect` pre-turn work run as
    /// hosted sessions (one-execution-engine E4). Without this, face-agent mode has no `delegate`
    /// tool and non-`ExecuteDirect` classifications fall through as plain answers about the failure.
    pub fn with_goal_hub(mut self, hub: Arc<GoalSessionHub>) -> Self {
        self.goals = Some(hub);
        self.rebuild_face_bridge();
        self
    }

    /// Enable face-agent / human-interfacer mode (built-in `delegate` tool; no pre-turn fleet).
    pub fn with_delegation_mode(mut self, enabled: bool) -> Self {
        self.delegation_mode = enabled;
        if enabled && self.system_prompt == DEFAULT_SYSTEM_PROMPT {
            self.system_prompt = HUMAN_INTERFACE_SYSTEM_PROMPT.to_string();
        }
        self.rebuild_face_bridge();
        self
    }
}
