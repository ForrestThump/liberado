//! Model thinking on a chat row: history reload and the live turn's finish.

use super::ChatMsg;

/// Drop blank thinking so an answer-only reply has no disclosure.
pub(super) fn kept(reasoning: Option<&str>) -> Option<String> {
    let text = reasoning?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Copy stored thinking onto the assistant rows already on screen, and append a
/// row when the turn was think-only and the stream never opened one.
pub(super) fn merge_history(live: &mut Vec<ChatMsg>, history: &[ChatMsg]) {
    let saved: Vec<&ChatMsg> = history.iter().filter(|m| m.role == "assistant").collect();
    let live_at: Vec<usize> = live
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "assistant")
        .map(|(i, _)| i)
        .collect();
    let shared = live_at.len().min(saved.len());
    for i in 0..shared {
        if live[live_at[i]].reasoning.is_none() {
            live[live_at[i]].reasoning = saved[i].reasoning.clone();
        }
    }
    if saved.len() <= live_at.len() {
        return;
    }
    for extra in &saved[live_at.len()..] {
        push_saved(live, extra);
    }
}

fn push_saved(live: &mut Vec<ChatMsg>, extra: &ChatMsg) {
    if extra.reasoning.is_none() && extra.content.is_empty() {
        return;
    }
    let mut msg = super::ChatMsg::new_assistant(extra.content.clone());
    msg.reasoning = extra.reasoning.clone();
    live.push(msg);
}

/// After `session_finished`, re-read the conversation and attach stored thinking.
/// The answer was already streamed; this only fills the collapsed step.
#[cfg(target_arch = "wasm32")]
pub(super) fn adopt_when_finished(
    api_base: String,
    session: dioxus::prelude::Signal<Option<String>>,
    mut messages: dioxus::prelude::Signal<Vec<ChatMsg>>,
) {
    use dioxus::prelude::{ReadableExt, WritableExt};
    let Some(id) = session.read().clone() else {
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(loaded) = super::fetch_conversation(&api_base, &id).await {
            messages.with_mut(|live| merge_history(live, &loaded.messages));
        }
    });
}
