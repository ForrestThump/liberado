//! Chat-surface `agent_profiles` field + builder on [`ChatSessions`].
//!
//! Kept off `sessions.rs` so that file stays at its cyclomatic / ploc baseline.
//! Spec: `docs/spec/architecture/chat-agent-surface-mode.md`.

use liberado_conversation_store::AgentProfiles;

use super::ChatSessions;

impl ChatSessions {
    /// Override the chat-surface `agent_profiles` set without this, the
    /// built-in conservative set is used (`coding`, `life`, `researcher`,
    /// `operator`). Tunable per deployment via
    /// `config.example/tuning.toml [chat] agent_profiles`.
    pub fn with_agent_profiles(mut self, profiles: AgentProfiles) -> Self {
        self.agent_profiles = profiles;
        self
    }
}
