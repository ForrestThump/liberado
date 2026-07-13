//! TUI-specific conversion from the shared SSE decoder's events into [`Action`].
//!
//! The incremental parser itself (`SseDecoder`/`SseEvent`) lives in
//! `chat_client_contract::native` — it's shared with the `liberado chat` CLI client, which
//! previously carried its own separate copy. Only the conversion to this crate's `Action` enum
//! is TUI-specific and stays here.

pub use chat_client_contract::native::{SseDecoder, SseEvent};

use chat_client_contract::{SessionEvent, SessionEventKind};

use crate::app::{Action, GoalUiEvent};

/// Convert a decoded [`SseEvent`] into the corresponding [`Action`] variant for
/// [`App::update()`](crate::app::App::update).
///
/// A local trait (rather than an inherent `impl SseEvent`) because `SseEvent` is defined in
/// `chat_client_contract`, not this crate — Rust's orphan rules only allow implementing a
/// *local* trait for a foreign type.
pub trait ToAction {
    /// Delegates JSON parsing to [`SessionEvent::from_sse_data`] (the shared wire helper for
    /// both the chat stream and the goal-session stream — the converged vocabulary).
    ///
    /// **Semantics:**
    /// - Unknown/future event types → `Ok(Action::SseToken(String::new()))` (benign no-op)
    /// - Goal-session-only kinds the chat view doesn't render (roles, progress, guards) →
    ///   the same benign no-op, until the TUI grows a session panel
    /// - Malformed JSON for a known structured event → `Err(description)`
    fn to_action(&self) -> Result<Action, String>;
}

impl ToAction for SseEvent {
    fn to_action(&self) -> Result<Action, String> {
        match SessionEvent::from_sse_data(&self.event, &self.data) {
            Ok(event) => Ok(match event.kind {
                SessionEventKind::Session { id } => Action::SseSession(id),
                SessionEventKind::Token { text } => Action::SseToken(text),
                SessionEventKind::ToolStarted { name, args_preview } => Action::SseTool {
                    name,
                    args: args_preview,
                },
                SessionEventKind::ToolFinished {
                    name,
                    ok,
                    result_preview,
                } => Action::SseToolResult {
                    name,
                    ok,
                    preview: result_preview,
                },
                SessionEventKind::SessionFinished { .. } => Action::SseDone,
                SessionEventKind::Failed { message } => Action::SseFailed(message),
                // The chat turn offered a specialist session — render a joinable affordance.
                SessionEventKind::SessionOffered {
                    id,
                    domain,
                    description,
                } => Action::GoalOffered {
                    session_id: id,
                    domain,
                    description,
                },
                // Goal-session-only kinds — not rendered in the chat view (yet; session-focus S3
                // adds the session panel that renders these, incl. AwaitingInput prompts).
                SessionEventKind::SessionStarted { .. }
                | SessionEventKind::RoleStarted { .. }
                | SessionEventKind::RoleFinished { .. }
                | SessionEventKind::Progress { .. }
                | SessionEventKind::AwaitingInput { .. }
                | SessionEventKind::HumanInput { .. }
                | SessionEventKind::ValidationFinished { .. }
                | SessionEventKind::LoopGuard { .. } => Action::SseToken(String::new()),
            }),
            Err(e) => Err(format!("malformed SSE data ({e}): {}", self.data)),
        }
    }
}

/// Decode one **goal-session** stream frame into a [`GoalUiEvent`] for the joined view. Unlike
/// [`ToAction`] (the chat path, which collapses session-only kinds to no-ops), this keeps the full
/// session vocabulary — roles, progress, validation, guards, and the `AwaitingInput`/`HumanInput`
/// pair the interactive focus model is built on.
///
/// - `Ok(Some(ev))` — a frame the joined view renders.
/// - `Ok(None)` — a benign frame with nothing to show (`RoleFinished`, a stray chat `Session`
///   head, or an unknown/future kind).
/// - `Err(desc)` — malformed JSON for a known structured event.
pub fn to_goal_event(event: &SseEvent) -> Result<Option<GoalUiEvent>, String> {
    let parsed = SessionEvent::from_sse_data(&event.event, &event.data)
        .map_err(|e| format!("malformed session SSE data ({e}): {}", event.data))?;
    Ok(match parsed.kind {
        SessionEventKind::SessionStarted { description, .. } => {
            Some(GoalUiEvent::Started { description })
        }
        SessionEventKind::Token { text } => Some(GoalUiEvent::Token(text)),
        SessionEventKind::ToolStarted { name, args_preview } => Some(GoalUiEvent::ToolStarted {
            name,
            args: args_preview,
        }),
        SessionEventKind::ToolFinished {
            name,
            ok,
            result_preview,
        } => Some(GoalUiEvent::ToolFinished {
            name,
            ok,
            preview: result_preview,
        }),
        SessionEventKind::RoleStarted { role, model } => Some(GoalUiEvent::Role {
            role,
            model: Some(model),
        }),
        SessionEventKind::Progress { message } => Some(GoalUiEvent::Progress(message)),
        SessionEventKind::ValidationFinished { ok, summary } => {
            Some(GoalUiEvent::Validation { ok, summary })
        }
        SessionEventKind::LoopGuard { guard, action } => {
            Some(GoalUiEvent::LoopGuard(format!("{guard} → {action}")))
        }
        SessionEventKind::AwaitingInput { prompt, options } => {
            Some(GoalUiEvent::Awaiting { prompt, options })
        }
        SessionEventKind::HumanInput { text } => Some(GoalUiEvent::Human(text)),
        SessionEventKind::SessionFinished { status, summary } => {
            Some(GoalUiEvent::Finished { status, summary })
        }
        // A hard failure is terminal — render it as a finished session with a failed status.
        SessionEventKind::Failed { message } => Some(GoalUiEvent::Finished {
            status: "failed".into(),
            summary: message,
        }),
        // Nothing to render on a goal session's own stream: role end, the chat-only session head,
        // and `session_offered` (a chat-stream affordance, not a pack event).
        SessionEventKind::RoleFinished { .. }
        | SessionEventKind::Session { .. }
        | SessionEventKind::SessionOffered { .. } => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_started_event_converts_to_sse_tool_action() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(
            "event: tool_started\ndata: {\"name\":\"search\",\"args_preview\":\"q=test\"}\n\n",
        );
        let action = events[0].to_action().unwrap();
        assert!(matches!(action, Action::SseTool { name, .. } if name == "search"));
    }

    #[test]
    fn tool_finished_event_converts_to_sse_tool_result_action() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: tool_finished\ndata: {\"name\":\"search\",\"ok\":true,\"result_preview\":\"3 results\"}\n\n");
        let action = events[0].to_action().unwrap();
        assert!(matches!(action, Action::SseToolResult { name, ok: true, .. } if name == "search"));
    }

    #[test]
    fn session_finished_event_converts_to_done() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push("event: session_finished\ndata: {\"status\":\"done\",\"summary\":\"\"}\n\n");
        assert!(matches!(events[0].to_action().unwrap(), Action::SseDone));
    }

    #[test]
    fn goal_session_frame_with_envelope_decodes_too() {
        // A goals-stream frame: full JSON including the serde tag + envelope fields.
        let mut decoder = SseDecoder::default();
        let events = decoder.push(
            "event: tool_started\ndata: {\"session_id\":\"g1\",\"at\":\"2026-07-11T00:00:00Z\",\"type\":\"tool_started\",\"name\":\"write_note\",\"args_preview\":\"...\"}\n\n",
        );
        let action = events[0].to_action().unwrap();
        assert!(matches!(action, Action::SseTool { name, .. } if name == "write_note"));
    }

    #[test]
    fn tool_started_malformed_json_returns_err() {
        let event = SseEvent {
            event: "tool_started".to_string(),
            data: "not valid json".to_string(),
        };
        let result = event.to_action();
        assert!(result.is_err(), "expected Err for malformed JSON");
        let err = result.unwrap_err();
        assert!(
            err.contains("malformed SSE data"),
            "error should mention malformed SSE data: {err}"
        );
    }

    #[test]
    fn tool_finished_malformed_json_returns_err() {
        let event = SseEvent {
            event: "tool_finished".to_string(),
            data: "{broken}".to_string(),
        };
        let result = event.to_action();
        assert!(result.is_err(), "expected Err for malformed JSON");
    }

    // ── goal-session stream mapping (to_goal_event) ──────────────────────────

    #[test]
    fn goal_awaiting_input_maps_to_awaiting() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(
            "event: awaiting_input\ndata: {\"prompt\":\"title?\",\"options\":[\"a\",\"b\"]}\n\n",
        );
        let ev = to_goal_event(&events[0]).unwrap().unwrap();
        assert!(matches!(
            ev,
            GoalUiEvent::Awaiting { prompt, options } if prompt == "title?" && options.len() == 2
        ));
    }

    #[test]
    fn goal_human_input_maps_to_human() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: human_input\ndata: {\"text\":\"Weekly Review\"}\n\n");
        let ev = to_goal_event(&events[0]).unwrap().unwrap();
        assert!(matches!(ev, GoalUiEvent::Human(t) if t == "Weekly Review"));
    }

    #[test]
    fn goal_session_finished_maps_to_finished() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push(
            "event: session_finished\ndata: {\"status\":\"succeeded\",\"summary\":\"done\"}\n\n",
        );
        let ev = to_goal_event(&events[0]).unwrap().unwrap();
        assert!(matches!(ev, GoalUiEvent::Finished { status, .. } if status == "succeeded"));
    }

    #[test]
    fn goal_role_finished_is_benign_none() {
        let mut decoder = SseDecoder::default();
        let events = decoder.push("event: role_finished\ndata: {\"role\":\"worker\"}\n\n");
        assert!(to_goal_event(&events[0]).unwrap().is_none());
    }
}
