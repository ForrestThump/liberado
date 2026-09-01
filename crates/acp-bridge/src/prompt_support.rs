use super::*;

pub(super) async fn preserved_worktree_report(workspace: &std::path::Path, label: &str) -> String {
    match coding_run::preserve_worktree(workspace, label).await {
        Ok(Some(sha)) => format!(
            "\n**Committed:** `{sha}` on `{}`\n",
            state_branch(workspace)
        ),
        Ok(None) => String::new(),
        Err(error) => format!("\n**Could not preserve work:** {error}\n"),
    }
}

pub(super) fn emit_finish_report(
    sink: &dyn WireSink,
    sid: &str,
    outcome: Option<&str>,
    preserved: &str,
) -> Result<(), String> {
    if let Some(report) = outcome {
        emit_agent_text_chunk(sink, sid, report)?;
    }
    emit_agent_text_chunk(sink, sid, preserved)
}

pub(super) fn drain_coding_events(
    sink: &dyn WireSink,
    sid: &str,
    events: &mut mpsc::Receiver<liberado_session::SessionEvent>,
    pending_tool_ids: &mut Vec<(String, String)>,
) -> Result<(), String> {
    while let Ok(event) = events.try_recv() {
        render_coding_event(sink, sid, &event, pending_tool_ids)?;
    }
    Ok(())
}

pub(super) async fn persist_coding_state(
    bridge: &Bridge,
    sid: &str,
    state: coding_run::CodingSessionState,
) {
    if let Some(session) = bridge.acp_sessions.lock().await.get_mut(sid) {
        session.coding = state;
    }
}

pub(super) fn coding_verdict(
    sink: &dyn WireSink,
    sid: &str,
    outcome: Result<coding_run::CodingRoundOutcome, String>,
) -> Result<(&'static str, Option<String>), String> {
    match outcome {
        Ok(result) => Ok(("done", Some(result.render()))),
        Err(error) => {
            emit_agent_text_chunk(sink, sid, &format!("\n**Coding pack error:** {error}\n"))?;
            Ok(("failed", None))
        }
    }
}

pub(super) fn render_face_prompt_result(
    sink: &dyn WireSink,
    sid: &str,
    result: Option<Result<(), String>>,
) -> Result<Value, String> {
    match result {
        None => {
            let _ = emit_agent_text_chunk(sink, sid, "\n*(cancelled)*\n");
            Ok(json!({ "stopReason": "cancelled" }))
        }
        Some(Ok(())) => Ok(json!({ "stopReason": "end_turn" })),
        Some(Err(error)) => {
            emit_agent_text_chunk(sink, sid, &format!("\n**Face mode error:** {error}\n"))?;
            Ok(json!({ "stopReason": "end_turn" }))
        }
    }
}
