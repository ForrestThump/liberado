//! Helpers for `ensure_converse` so that function stays at or below its
//! cyclomatic baseline. Allocation and identity stay in `workspace_targets`.

use std::sync::Arc;

use super::*;

/// Reused coding handles must not inherit another session's process-wide
/// `CARGO_TARGET_DIR`. Chat reuse does not touch that variable.
pub(super) fn reapply_coding_workspace_targets(
    bridge: &Bridge,
    mode: AgentMode,
    cwd: &std::path::Path,
) {
    if mode.uses_coding_tools() {
        workspace_targets::apply_workspace_targets(&bridge.coder_tuning.workspace_build, cwd);
    }
}

pub(super) fn stored_converse_turns(
    sid: &str,
    live_history: Vec<session_store::StoredMessage>,
) -> Vec<session_store::StoredMessage> {
    if !live_history.is_empty() {
        return live_history;
    }
    session_store::load(sid)
        .ok()
        .flatten()
        .map(|r| r.messages)
        .unwrap_or_default()
}

pub(super) async fn open_converse_handle(
    bridge: &Bridge,
    sid: &str,
    mode: AgentMode,
    cwd: &std::path::Path,
    stored: &[session_store::StoredMessage],
) -> Result<SessionHandle, String> {
    if mode.uses_coding_tools() {
        let permission = permission_attach(bridge, sid, cwd);
        let parts = interactive::prepare_coding_converse(
            cwd,
            sid,
            &bridge.coder_tuning,
            ask_human::may_ask_human(&bridge.local_grant),
            bridge.config_dir.as_deref(),
            permission,
        )
        .await?;
        return Ok(open_handle(
            sid,
            Arc::clone(&bridge.provider),
            bridge.max_turns,
            parts.system,
            stored,
            parts.tools,
            true,
        ));
    }
    Ok(open_handle(
        sid,
        Arc::clone(&bridge.provider),
        bridge.max_turns,
        chat_system_prompt(cwd, bridge.system_prompt.as_deref()),
        stored,
        Arc::new(NoTools),
        false,
    ))
}
