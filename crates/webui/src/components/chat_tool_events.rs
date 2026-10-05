//! Live `tool_finished` for one chat turn.
//!
//! Closes the pending thinking step, and asks the sidebar to refetch when Agent
//! Creator's `create_agent` succeeded. The user stays on the creator session, so
//! the open-conversation id does not change and would not reload the list.

use chat_client_contract::{SessionEvent, SessionEventKind};
use dioxus::prelude::*;

use super::ChatMsg;

pub(super) fn note_tool_finished(
    data: &str,
    mut messages: Signal<Vec<ChatMsg>>,
    mut list_epoch: Signal<u64>,
) {
    let Ok(SessionEvent {
        kind:
            SessionEventKind::ToolFinished {
                name,
                ok,
                result_preview: preview,
            },
        ..
    }) = SessionEvent::from_sse_data("tool_finished", data)
    else {
        return;
    };
    if crate::components::sidebar_shelf::refreshes_agent_shelf(&name, ok) {
        list_epoch += 1;
    }
    messages.with_mut(|m| {
        let found = m
            .iter_mut()
            .rev()
            .filter(|msg| msg.role == "assistant")
            .find_map(|msg| {
                msg.thinking_steps
                    .iter_mut()
                    .rev()
                    .find(|step| step.ok.is_none() && step.tool_name == name)
            });
        if let Some(step) = found {
            step.ok = Some(ok);
            step.preview = preview;
        }
    });
}
