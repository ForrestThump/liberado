//! Chat-surface `agent_profiles` builder + accessor on [`SessionStore`].
//!
//! Kept off `jsonl.rs` so that file stays at its cyclomatic / ploc baseline.
//! Spec: `docs/spec/architecture/chat-agent-surface-mode.md`.

use liberado_conversation_store::AgentProfiles;

use super::SessionStore;

impl SessionStore {
    /// Override the chat-surface `agent_profiles` set used by the chat-lens
    /// projection on every read. Threaded through from
    /// `config.tuning.chat.agent_profiles` by the daemon wiring. Without
    /// this, the projection uses the conservative built-in default. Spec:
    /// `docs/spec/architecture/chat-agent-surface-mode.md`.
    pub fn with_agent_profiles(mut self, profiles: AgentProfiles) -> Self {
        self.agent_profiles = profiles;
        self
    }

    /// The `agent_profiles` set currently in force on this store.
    pub fn agent_profiles(&self) -> &AgentProfiles {
        &self.agent_profiles
    }
}
