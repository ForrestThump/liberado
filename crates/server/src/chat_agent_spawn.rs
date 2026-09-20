//! Wire privileged `create_agent` onto [`ChatSessions`] at daemon boot.
//!
//! Kept off `lib.rs` so the boot file stays at its function-count baseline: the
//! profile→grant closure would otherwise count as an extra function there.

use std::sync::Arc;

use liberado_bootstrap::Config;
use liberado_main_agent::ChatSessions;
use liberado_session::SessionGrant;

/// Attach the fail-closed profile resolver and install the weak self-handle
/// used by the face `create_agent` tool.
pub(crate) fn install(sessions: ChatSessions, config: &Config) -> Arc<ChatSessions> {
    let config_for_resolve = config.clone();
    let sessions = sessions.with_profile_resolver(Arc::new(move |name: &str| {
        match config_for_resolve.resolve_session_profile(Some(name), "") {
            Ok(resolved) => {
                let parts = resolved.grant_parts();
                Ok(SessionGrant {
                    capabilities: parts.capabilities,
                    profile: parts.profile,
                    overrides: serde_json::to_value(&resolved.overrides)
                        .unwrap_or(serde_json::Value::Null),
                    delegation: parts.delegation,
                    model: parts.model.map(str::to_string),
                    prompt_append: parts.prompt_append.map(str::to_string),
                })
            }
            Err(e) => Err(e.to_string()),
        }
    }));
    let sessions = Arc::new(sessions);
    sessions.install_self_handle();
    sessions
}
