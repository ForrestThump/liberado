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
///
/// Serialization of the resolved overrides fails closed: a `serde_json` bug must
/// not silently downgrade the child to default overrides (which would be wider
/// than the named profile specified). The resolver returns the error and the
/// caller refuses the create — same fail-closed shape as an unknown profile name.
pub(crate) fn install(sessions: ChatSessions, config: &Config) -> Arc<ChatSessions> {
    let config_for_resolve = config.clone();
    let sessions = sessions.with_profile_resolver(Arc::new(move |name: &str| {
        match config_for_resolve.resolve_session_profile(Some(name), "") {
            Ok(resolved) => {
                let parts = resolved.grant_parts();
                let overrides = serde_json::to_value(&resolved.overrides)
                    .map_err(|e| format!("failed to serialize profile overrides: {e}"))?;
                Ok(SessionGrant {
                    capabilities: parts.capabilities,
                    profile: parts.profile,
                    overrides,
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
