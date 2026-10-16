//! Reload the editor transcript while the Automations page is open.
//!
//! Approve writes the model's note after the card tap returns. This poll
//! picks that note up, including a refusal when the config file is read-only.
//!
//! Scrolling follows the latest row only while the reader is near the bottom,
//! or just after they send. A poll must not pull them back down the thread.

use std::cell::{Cell, RefCell};

use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::super::chat::ChatMsg;
use super::logic::{self, EDITOR_POLL_MS, SCROLL_TRIGGER_POLL, SCROLL_TRIGGER_RENDER};
use super::net as automation_editor_net;

thread_local! {
    static POLL_ID: Cell<Option<i32>> = const { Cell::new(None) };
    static POLL_CB: RefCell<Option<Closure<dyn FnMut()>>> = const { RefCell::new(None) };
    static SCROLL_WHY: Cell<&'static str> = const { Cell::new(SCROLL_TRIGGER_RENDER) };
    static SUPPRESS_SCROLL: Cell<bool> = const { Cell::new(false) };
    static BOUND_TRANSCRIPT: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
    static LAST_HOLD: Cell<Option<(&'static str, bool)>> = const { Cell::new(None) };
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
                mark_poll();
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

pub(super) fn take_scroll_why() -> &'static str {
    SCROLL_WHY.replace(SCROLL_TRIGGER_RENDER)
}

pub(super) fn scroll_transcript(follow: bool, why: &'static str) {
    let Some(node) = editor_transcript() else {
        return;
    };
    let top = node.scroll_top();
    let near = near_edge(&node);
    if follow {
        pin_bottom(&node, why, top, near);
        return;
    }
    log_scroll(why, top, near, false, "hold");
}

pub(super) fn watch_editor_scroll(follow: Signal<bool>) {
    let Some(node) = editor_transcript() else {
        retry_watch(follow);
        return;
    };
    if transcript_is_bound(&node) {
        return;
    }
    bind_editor_scroll(node, follow);
}

fn mark_poll() {
    SCROLL_WHY.set(SCROLL_TRIGGER_POLL);
}

fn editor_transcript() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .get_element_by_id("automation-editor-transcript")
}

fn near_edge(node: &web_sys::Element) -> bool {
    logic::editor_near_bottom(
        node.scroll_top(),
        node.client_height(),
        node.scroll_height(),
    )
}

fn pin_bottom(node: &web_sys::Element, why: &'static str, top: i32, near: bool) {
    let target = bottom_scroll(node);
    if top >= target {
        return;
    }
    log_scroll(why, top, near, true, "bottom");
    suppress_scroll_note();
    node.set_scroll_top(node.scroll_height());
    release_scroll_note();
}

fn bottom_scroll(node: &web_sys::Element) -> i32 {
    node.scroll_height()
        .saturating_sub(node.client_height())
        .max(0)
}

fn transcript_is_bound(node: &web_sys::Element) -> bool {
    BOUND_TRANSCRIPT.with(|cell| match cell.borrow().as_ref() {
        Some(bound) => bound.is_same_node(Some(node)),
        None => false,
    })
}

fn bind_editor_scroll(node: web_sys::Element, follow: Signal<bool>) {
    let callback = Closure::<dyn FnMut()>::new(move || note_editor_scroll(follow));
    let Ok(_) = node.add_event_listener_with_callback("scroll", callback.as_ref().unchecked_ref())
    else {
        return;
    };
    callback.forget();
    BOUND_TRANSCRIPT.with(|cell| *cell.borrow_mut() = Some(node));
}

fn note_editor_scroll(mut follow: Signal<bool>) {
    if SUPPRESS_SCROLL.with(Cell::get) {
        return;
    }
    let Some(node) = editor_transcript() else {
        return;
    };
    let near = near_edge(&node);
    let stick = logic::should_autoscroll(near, false);
    if *follow.peek() == stick {
        return;
    }
    follow.set(stick);
    let action = stick_action(stick);
    log_scroll(
        SCROLL_TRIGGER_RENDER,
        node.scroll_top(),
        near,
        stick,
        action,
    );
}

fn stick_action(stick: bool) -> &'static str {
    if stick { "stick" } else { "release" }
}

fn retry_watch(follow: Signal<bool>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(move |_stamp: f64| bind_if_present(follow));
    if window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .is_ok()
    {
        callback.forget();
    }
}

fn bind_if_present(follow: Signal<bool>) {
    let Some(node) = editor_transcript() else {
        return;
    };
    if transcript_is_bound(&node) {
        return;
    }
    bind_editor_scroll(node, follow);
}

fn suppress_scroll_note() {
    SUPPRESS_SCROLL.with(|cell| cell.set(true));
}

fn release_scroll_note() {
    let Some(window) = web_sys::window() else {
        SUPPRESS_SCROLL.with(|cell| cell.set(false));
        return;
    };
    let callback = Closure::once(move |_stamp: f64| {
        SUPPRESS_SCROLL.with(|cell| cell.set(false));
    });
    if window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .is_err()
    {
        SUPPRESS_SCROLL.with(|cell| cell.set(false));
        return;
    }
    callback.forget();
}

fn log_scroll(why: &'static str, top: i32, near: bool, follow: bool, action: &'static str) {
    if skip_repeat_hold(why, near, action) {
        return;
    }
    web_sys::console::log_1(
        &format!(
            "[automation-scroll] trigger={why} scrollTop={top} nearBottom={near} follow={follow} action={action}"
        )
        .into(),
    );
}

fn skip_repeat_hold(why: &'static str, near: bool, action: &'static str) -> bool {
    if action != "hold" {
        LAST_HOLD.with(|cell| cell.set(None));
        return false;
    }
    let repeat = LAST_HOLD.with(|cell| cell.get() == Some((why, near)));
    LAST_HOLD.with(|cell| cell.set(Some((why, near))));
    repeat
}

fn clear_poll() {
    if let Some(id) = POLL_ID.take()
        && let Some(window) = web_sys::window()
    {
        window.clear_interval_with_handle(id);
    }
    POLL_CB.with(|cell| *cell.borrow_mut() = None);
}
