//! Wire chat-surface boot settings onto [`ChatSessions`] at daemon boot.
//!
//! Two small attachers live here so the boot file stays at its function-count baseline:
//!
//! * [`install`] (privileged `create_agent`) — the profile→grant closure and the weak
//!   self-handle the face tool needs.
//! * [`apply_user_timezone`] — the operator IANA timezone, with a fail-soft shape that
//!   mirrors `apply_timezone` in `liberado-bootstrap`.
//!
//! Pulling the timezone wiring out of `lib.rs` keeps `build_chat`'s cognitive complexity
//! under the workspace clippy gate and the file's function count under the module-health
//! gate. The two helpers share a module because both are "one `with_*` call on a
//! `ChatSessions` at boot" — small, fail-closed wrappers that exist to keep the boot path
//! readable.

use std::sync::Arc;

use liberado_bootstrap::Config;
use liberado_main_agent::ChatSessions;
use liberado_session::SessionGrant;

/// Wire the operator IANA timezone onto the chat session. A missing or invalid name is
/// logged and the chat runs without wall-clock context — `Config::validate` already
/// refused unknown zones at load, but mirroring `apply_timezone`'s fail-soft shape means
/// a misconfigured `topology.timezone` does not refuse to start the chat.
pub(crate) fn apply_user_timezone(sessions: ChatSessions, config: &Config) -> ChatSessions {
    match config.topology.user_timezone() {
        Ok(tz) => {
            tracing::info!(timezone = %tz.iana_name(), "chat: operator timezone configured");
            sessions.with_user_timezone(tz)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "chat: topology.timezone invalid — turns will not get Local time stamps"
            );
            sessions
        }
    }
}

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
