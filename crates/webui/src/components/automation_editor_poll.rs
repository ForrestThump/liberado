//! Reload the editor transcript while the Automations page is open.
//!
//! Approve writes the model's note after the card tap returns. This poll
//! picks that note up, including a refusal when the config file is read-only.

use std::cell::{Cell, RefCell};

use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::super::chat::ChatMsg;
use super::logic::EDITOR_POLL_MS;
use super::net as automation_editor_net;

thread_local! {
    static POLL_ID: Cell<Option<i32>> = const { Cell::new(None) };
    static POLL_CB: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
}

pub(super) fn stop() {
    clear_poll();
}

pub(super) fn restart(
    api_base: String,
    session_id: Option<String>,
    messages: Signal<Vec<ChatMsg>>,
    pending_ids: Signal<Vec<String>>,
) {
    clear_poll();
    let Some(session_id) = session_id else {
        return;
    };
    let callback = Closure::wrap(Box::new(move || {
        let base = api_base.clone();
        let session = session_id.clone();
        let mut messages = messages;
        let mut pending_ids = pending_ids;
        wasm_bindgen_futures::spawn_local(async move {
            if let Ok(snapshot) = automation_editor_net::load_snapshot(&base, &session).await {
                messages.set(snapshot.messages);
                pending_ids.set(snapshot.pending_ids);
            }
        });
    }) as Box<dyn FnMut()>);
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(id) = window.set_interval_with_callback_and_timeout_and_arguments_0(
        callback.as_ref().unchecked_ref(),
        EDITOR_POLL_MS,
    ) else {
        return;
    };
    POLL_ID.set(Some(id));
    POLL_CB.with(|cell| *cell.borrow_mut() = Some(callback));
}

pub(super) fn scroll_dock() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(node) = window
        .document()
        .and_then(|document| document.get_element_by_id("automation-editor-transcript"))
    else {
        return;
    };
    node.set_scroll_top(node.scroll_height());
}

fn clear_poll() {
    if let Some(id) = POLL_ID.take()
        && let Some(window) = web_sys::window()
    {
        window.clear_interval_with_handle(id);
    }
    POLL_CB.with(|cell| *cell.borrow_mut() = None);
}
