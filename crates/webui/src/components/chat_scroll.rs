//! Keep `#chat-messages` on the latest row.
//!
//! Opening an agent or a chat fetches history after the pane is on screen. Without a
//! pin, that paint sits at the top of the thread — the oldest row — and stays there
//! when the rows arrive. A send has the same hole from the other direction: a reader
//! who scrolled up to earlier messages would keep that place, and the line they just
//! sent would land below the fold.
//!
//! The pin is the whole policy. Opening a thread sets it, including when the fetch
//! returns later. Sending sets it again, even if the reader had moved up. Live growth
//! (a token, the thinking mark) follows only while the pin is set. A scroll that
//! leaves the bottom clears it, so a reply does not pull the reader back down.

#[cfg(target_arch = "wasm32")]
use std::cell::{Cell, RefCell};

use dioxus::prelude::*;

use super::ChatMsg;

/// How far from the bottom edge still counts as "on the latest row".
///
/// Sub-pixel layout and the copy-button nudge sit inside this window. Past it,
/// the reader has moved up into earlier messages and live growth must hold.
pub(super) const LATEST_SLACK_PX: i32 = 8;

/// Whether the next content change moves the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TranscriptJump {
    /// `scrollTop` becomes the bottom edge of `#chat-messages`.
    Bottom,
    /// Leave the reader where they are.
    Hold,
}

/// Stickiness of the open thread.
///
/// Starts pinned: a thread that has not been scrolled yet should show its end,
/// and the first history payload is that end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptStick {
    pinned: bool,
}

impl TranscriptStick {
    pub(super) fn pinned() -> Self {
        Self { pinned: true }
    }

    /// The open conversation's history is now the transcript on screen.
    ///
    /// Called when that payload is applied, not when the id changes. The id
    /// changes first; the rows arrive later. Pinning on the payload is what
    /// puts the latest row in view after the async load. Agents and chats share
    /// this path: both are a conversation id on the same pane.
    pub(super) fn opened_conversation(&mut self) {
        self.pinned = true;
    }

    /// The reader sent. Jump even if they had scrolled up.
    pub(super) fn user_sent(&mut self) {
        self.pinned = true;
    }

    /// Record where the pane sits after a reader scroll.
    pub(super) fn note_viewport(
        &mut self,
        scroll_top: i32,
        client_height: i32,
        scroll_height: i32,
    ) {
        self.pinned = viewport_shows_latest(scroll_top, client_height, scroll_height);
    }

    pub(super) fn on_content(&self) -> TranscriptJump {
        if self.pinned {
            TranscriptJump::Bottom
        } else {
            TranscriptJump::Hold
        }
    }
}

/// True when the latest row — human or agent — is in view.
///
/// The two sides share one pane. The anchor is the bottom edge, not a bubble.
pub(super) fn viewport_shows_latest(
    scroll_top: i32,
    client_height: i32,
    scroll_height: i32,
) -> bool {
    let client = client_height.max(0);
    let height = scroll_height.max(0);
    if height <= client.saturating_add(LATEST_SLACK_PX) {
        return true;
    }
    scroll_top.saturating_add(client) >= height.saturating_sub(LATEST_SLACK_PX)
}

/// `scrollTop` that places the bottom edge of the content on the bottom edge of the pane.
pub(super) fn max_scroll_top(client_height: i32, scroll_height: i32) -> i32 {
    let client = if client_height < 0 { 0 } else { client_height };
    let height = if scroll_height < 0 { 0 } else { scroll_height };
    if height > client { height - client } else { 0 }
}

/// The `scrollTop` to write, or `None` when the pane should not move.
pub(super) fn scroll_top_to_apply(
    jump: TranscriptJump,
    scroll_top: i32,
    client_height: i32,
    scroll_height: i32,
) -> Option<i32> {
    if jump != TranscriptJump::Bottom {
        return None;
    }
    let target = max_scroll_top(client_height, scroll_height);
    if scroll_top >= target {
        None
    } else {
        Some(target)
    }
}

/// Watches the transcript and the reader's scroll, and moves the pane when the pin says so.
///
/// Rendered inside `#chat-messages` so it mounts with the pane. It draws nothing.
#[component]
pub(super) fn TranscriptScroll(
    mut follow: Signal<TranscriptStick>,
    messages: Signal<Vec<ChatMsg>>,
    sending: Signal<bool>,
) -> Element {
    use_effect(move || {
        let jump = follow.read().on_content();
        // Any write to the message list, including a token appended to the last row.
        let _rows = messages.read().len();
        let _sending = sending();
        if jump == TranscriptJump::Bottom {
            schedule_scroll_to_latest(follow);
        }
    });
    use_effect(move || {
        install_transcript_scroll_listener(follow);
    });
    rsx! {}
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static SUPPRESS_SCROLL_NOTE: Cell<u32> = const { Cell::new(0) };
    static SCROLL_PANE: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
}

pub(super) fn schedule_scroll_to_latest(follow: Signal<TranscriptStick>) {
    #[cfg(target_arch = "wasm32")]
    schedule_scroll_to_latest_wasm(follow);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = follow;
    }
}

fn install_transcript_scroll_listener(follow: Signal<TranscriptStick>) {
    #[cfg(target_arch = "wasm32")]
    install_transcript_scroll_listener_wasm(follow);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = follow;
    }
}

#[cfg(target_arch = "wasm32")]
fn schedule_scroll_to_latest_wasm(follow: Signal<TranscriptStick>) {
    // The listener effect can run before the pane is in the document. A later
    // jump retries the attach, so scrolling up still clears the pin.
    install_transcript_scroll_listener_wasm(follow);
    if apply_latest() != Apply::Already {
        retry_latest_on_frame(follow);
    }
}

#[cfg(target_arch = "wasm32")]
fn retry_latest_on_frame(follow: Signal<TranscriptStick>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(move |_timestamp: f64| {
        if follow.read().on_content() == TranscriptJump::Bottom {
            let _ = apply_latest();
        }
    });
    if window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .is_ok()
    {
        callback.forget();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg(target_arch = "wasm32")]
enum Apply {
    Already,
    Moved,
    Missing,
}

#[cfg(target_arch = "wasm32")]
fn apply_latest() -> Apply {
    let Some(pane) = messages_pane() else {
        return Apply::Missing;
    };
    let Some(target) = scroll_top_to_apply(
        TranscriptJump::Bottom,
        pane.scroll_top(),
        pane.client_height(),
        pane.scroll_height(),
    ) else {
        return Apply::Already;
    };
    begin_suppress();
    pane.set_scroll_top(target);
    end_suppress_on_frame();
    Apply::Moved
}

#[cfg(target_arch = "wasm32")]
fn listener_already_bound() -> bool {
    let Some(pane) = messages_pane() else {
        return false;
    };
    SCROLL_PANE.with(|cell| match cell.borrow().as_ref() {
        Some(bound) => bound.is_same_node(Some(&pane)),
        None => false,
    })
}

#[cfg(target_arch = "wasm32")]
fn install_transcript_scroll_listener_wasm(follow: Signal<TranscriptStick>) {
    attach_scroll_listener(follow, true);
}

#[cfg(target_arch = "wasm32")]
fn attach_scroll_listener(follow: Signal<TranscriptStick>, retry: bool) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    if listener_already_bound() {
        return;
    }
    let Some(pane) = messages_pane() else {
        if retry {
            retry_listener_on_frame(follow);
        }
        return;
    };
    let callback = Closure::<dyn FnMut()>::new(move || note_reader_scroll(follow));
    let Ok(_) = pane.add_event_listener_with_callback("scroll", callback.as_ref().unchecked_ref())
    else {
        return;
    };
    callback.forget();
    SCROLL_PANE.with(|cell| *cell.borrow_mut() = Some(pane));
}

#[cfg(target_arch = "wasm32")]
fn retry_listener_on_frame(follow: Signal<TranscriptStick>) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let callback = Closure::once(move |_timestamp: f64| {
        attach_scroll_listener(follow, false);
    });
    if window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .is_ok()
    {
        callback.forget();
    }
}

#[cfg(target_arch = "wasm32")]
fn note_reader_scroll(mut follow: Signal<TranscriptStick>) {
    if scroll_note_suppressed() {
        return;
    }
    let Some(pane) = messages_pane() else {
        return;
    };
    follow.write().note_viewport(
        pane.scroll_top(),
        pane.client_height(),
        pane.scroll_height(),
    );
}

#[cfg(target_arch = "wasm32")]
fn messages_pane() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .get_element_by_id("chat-messages")
}

#[cfg(target_arch = "wasm32")]
fn scroll_note_suppressed() -> bool {
    SUPPRESS_SCROLL_NOTE.with(Cell::get) > 0
}

#[cfg(target_arch = "wasm32")]
fn begin_suppress() {
    SUPPRESS_SCROLL_NOTE.with(|cell| cell.set(cell.get().saturating_add(1)));
}

#[cfg(target_arch = "wasm32")]
fn end_suppress_on_frame() {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        end_suppress();
        return;
    };
    let callback = Closure::once(move |_timestamp: f64| end_suppress());
    if window
        .request_animation_frame(callback.as_ref().unchecked_ref())
        .is_err()
    {
        end_suppress();
        return;
    }
    callback.forget();
}

#[cfg(target_arch = "wasm32")]
fn end_suppress() {
    SUPPRESS_SCROLL_NOTE.with(|cell| cell.set(cell.get().saturating_sub(1)));
}

#[cfg(test)]
#[path = "chat_scroll_tests.rs"]
mod chat_scroll_tests;
