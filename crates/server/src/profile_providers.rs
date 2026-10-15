//! Install per-profile inference backends on the chat face.
//!
//! The map is built once at boot from the loaded config. Each turn looks the
//! session's profile name up again, so a process started with an edited profile
//! sends an already-pinned model to the new provider.

use liberado_bootstrap::Config;
use liberado_main_agent::ChatSessions;

pub(crate) fn attach(sessions: ChatSessions, config: &Config) -> ChatSessions {
    let wired = liberado_bootstrap::profile_providers_from_config(config);
    if wired.routes.is_empty() {
        return sessions;
    }
    tracing::info!(
        profiles = wired.routes.len(),
        unavailable = wired.unavailable.len(),
        "chat: profile provider routes"
    );
    sessions.with_profile_providers(wired.routes, wired.backends, wired.unavailable)
}
