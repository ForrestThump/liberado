//! Install per-profile face turn ceilings on the chat face.
//!
//! Built once at boot from the loaded config, including disabled profiles, so a session that
//! still names a draft keeps the ceiling. Each turn looks the name up again.

use std::collections::HashMap;

use liberado_bootstrap::Config;
use liberado_main_agent::ChatSessions;

pub(crate) fn attach(sessions: ChatSessions, config: &Config) -> ChatSessions {
    let budgets: HashMap<String, u32> = config
        .topology
        .session_profiles
        .iter()
        .filter_map(|profile| profile.max_turns.map(|turns| (profile.name.clone(), turns)))
        .collect();
    if budgets.is_empty() {
        return sessions;
    }
    tracing::info!(profiles = budgets.len(), "chat: profile turn ceilings");
    sessions.with_profile_max_turns(budgets)
}
