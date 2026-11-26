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
//! (a token, the thinking mark) follows only while the pin is set. A scroll the reader
//! made that leaves the bottom clears it, so a reply does not pull the reader back down.
//!
//! **A scroll the browser made is not the reader's.** Replacing the rows can move the
//! pane on its own: the content shrinks and the browser clamps `scrollTop`, the
//! content above the viewport grows and the browser re-anchors it, or the rows land
//! in a fresh pane that starts at the top. The reader did not ask to leave the latest
//! row in any of those cases, and the pane goes back there. Only a scroll the reader
//! made — a wheel, a touch, a scrollbar drag, the keys that page the transcript —
//! releases the pane.
//!
//! **A refresh is not an open.** The history is fetched again when a turn ends, and
//! that payload is for the conversation the reader is already in. It owns nothing: a
//! reader who scrolled up while the reply streamed keeps that place when the reply
//! finishes. Only a conversation the reader opened or picked owns the bottom.

#[cfg(target_arch = "wasm32")]
use std::cell::{Cell, RefCell};

use dioxus::prelude::*;

use super::ChatMsg;

/// How far from the bottom edge still counts as "on the latest row".
///
/// Sub-pixel layout and the copy-button nudge sit inside this window. Past it,
/// the reader has moved up into earlier messages and live growth must hold.
pub(super) const LATEST_SLACK_PX: i32 = 8;

/// How long a reader gesture keeps the scrolls that follow it the reader's.
///
/// Long enough for a slow scrollbar drag and for a phone fling, short enough that
/// a stray click in the pane is not still counted when the browser moves the pane
/// half a minute later.
#[cfg(target_arch = "wasm32")]
const READER_GESTURE_WINDOW_MS: f64 = 500.0;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TranscriptStick {
    /// The pane should sit on the latest row.
    following: bool,
    /// The conversation whose history the pane is showing.
    conversation: Option<String>,
    /// The reader's own send put the conversation on screen.
    ///
    /// The first payload for it is the refresh at the end of that turn, not a
    /// conversation the reader just opened, so it owns nothing.
    sent: bool,
}

impl TranscriptStick {
    pub(super) fn pinned() -> Self {
        Self {
            following: true,
            conversation: None,
            sent: false,
        }
    }

    /// The history for `conv_id` is now the transcript on screen.
    ///
    /// A conversation the reader just opened owns the bottom of the pane: it must
    /// land on the latest message even if the reader had scrolled up in the thread
    /// they came from. A refresh of the conversation already on screen does not:
    /// the reader may have scrolled up while the reply was streaming, and the
    /// refresh must not drag them back down.
    ///
    /// The refresh at the end of the reader's own turn is the same thing. The
    /// conversation is the one their send opened, so it owns nothing either, and
    /// a reader who scrolled up while the reply streamed keeps that place.
    pub(super) fn opened_conversation(&mut self, conv_id: &str) {
        let is_new = payload_owns_bottom(conv_id, self.conversation.as_deref());
        self.conversation = Some(conv_id.to_string());
        if self.sent {
            // The reader's own send opened this conversation. The payload is the
            // refresh at the end of that turn: the reader keeps the place they
            // scrolled to.
            self.sent = false;
            return;
        }
        if !is_new {
            // A refresh of the conversation on screen. The reader keeps their place.
            return;
        }
        self.following = true;
    }

    /// The reader sent. Jump even if they had scrolled up.
    ///
    /// The turn ends in the conversation the send opens, and the refresh at the
    /// end of that turn must not take the bottom back from a reader who scrolled
    /// up while the reply streamed.
    pub(super) fn user_sent(&mut self) {
        self.following = true;
        self.sent = true;
    }

    /// The stream reported the conversation the reader's send opened.
    ///
    /// The pane is showing it now. This is not a pin: the reader may have scrolled
    /// up while the reply was already streaming.
    pub(super) fn note_sent_conversation(&mut self, conv_id: &str) {
        self.conversation = Some(conv_id.to_string());
        self.sent = true;
    }

    /// The pane is showing `conv_id` now.
    ///
    /// A conversation the reader picked is not the one their own send opened, so
    /// the refresh at the end of that turn does not apply to it: it is a
    /// conversation they opened, and it owns the bottom of the pane.
    ///
    /// A pane that was showing nothing is not a pick. The conversation on screen
    /// is the one their own send opened.
    pub(super) fn note_shown_conversation(&mut self, conv_id: Option<&str>) {
        if self.conversation.is_some() && self.conversation.as_deref() != conv_id {
            self.sent = false;
        }
    }

    /// The pane sits on the latest row.
    pub(super) fn note_at_bottom(&mut self) {
        self.following = true;
    }

    /// The reader scrolled away from the latest row. They keep that place, and
    /// live growth must not pull them back down.
    pub(super) fn note_reader_scroll(&mut self) {
        self.following = false;
    }

    /// The browser moved the pane — a content replace, a clamp, a re-anchor.
    ///
    /// The reader did not ask to leave the latest row, so the pin stands. The
    /// answer says where the pane belongs: `Bottom` puts it back on the latest
    /// row, `Hold` leaves a reader who scrolled up where they scrolled to.
    pub(super) fn note_browser_scroll(&self) -> TranscriptJump {
        self.on_content()
    }

    /// Whether the next content change moves the pane to the latest row.
    pub(super) fn on_content(&self) -> TranscriptJump {
        if self.following {
            TranscriptJump::Bottom
        } else {
            TranscriptJump::Hold
        }
    }
}

/// The keys that move the transcript.
///
/// A reader who pages up with the keyboard is reading an earlier message, so the
/// key is the reader's own move — the same release a wheel or a touch gives.
#[cfg(any(target_arch = "wasm32", test))]
pub(super) const READER_KEYS: [&str; 6] =
    ["PageUp", "PageDown", "Home", "End", "ArrowUp", "ArrowDown"];

/// Whether a loaded history payload takes the bottom of the pane.
///
/// A payload for a conversation the reader just opened owns the bottom: the pane must
/// land on the latest message even if the reader had scrolled up in the thread they
/// came from. A refresh of the conversation already on screen does not: the reader may
/// have scrolled up while the reply was streaming, and the refresh must not drag them
/// back down.
pub(super) fn payload_owns_bottom(loaded_conv: &str, shown_conv: Option<&str>) -> bool {
    shown_conv != Some(loaded_conv)
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
    /// The pane the scroll listener is bound to.
    static SCROLL_PANE: RefCell<Option<web_sys::Element>> = const { RefCell::new(None) };
    /// When the reader last scrolled the pane by hand, in milliseconds since the epoch.
    static READER_GESTURE_MS: Cell<f64> = const { Cell::new(0.0) };
}

pub(super) fn schedule_scroll_to_latest(follow: Signal<TranscriptStick>) {
    #[cfg(target_arch = "wasm32")]
    {
        // The listener effect can run before the pane is in the document. A later
        // jump retries the attach, so scrolling up still clears the pin.
        attach_scroll_listener(follow, true);
        if apply_latest() != Apply::Already {
            retry_latest_on_frame(follow);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = follow;
    }
}

fn install_transcript_scroll_listener(follow: Signal<TranscriptStick>) {
    #[cfg(target_arch = "wasm32")]
    attach_scroll_listener(follow, true);
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = follow;
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
    pane.set_scroll_top(target);
    Apply::Moved
}

#[cfg(target_arch = "wasm32")]
fn attach_scroll_listener(follow: Signal<TranscriptStick>, retry: bool) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    let Some(pane) = messages_pane() else {
        if retry {
            retry_listener_on_frame(follow);
        }
        return;
    };
    // The listener belongs to the pane it was installed on. A pane that has
    // been replaced is a different node, so the old listener stays with the old
    // pane and this one has to be installed again.
    let already_bound = SCROLL_PANE.with(|cell| match cell.borrow().as_ref() {
        Some(bound) => bound.is_same_node(Some(&pane)),
        None => false,
    });
    if already_bound {
        return;
    }
    let callback = Closure::<dyn FnMut()>::new(move || note_pane_scroll(follow));
    let Ok(_) = pane.add_event_listener_with_callback("scroll", callback.as_ref().unchecked_ref())
    else {
        return;
    };
    callback.forget();
    attach_reader_gesture_listeners(&pane);
    SCROLL_PANE.with(|cell| *cell.borrow_mut() = Some(pane));
}

/// Mark the scrolls the reader makes, so a move the browser makes is not read as one.
///
/// A wheel, a touch, a scrollbar drag, and the keys that page the transcript are
/// the ways a reader scrolls a pane. A click is not: it is left out on purpose,
/// because a click in the pane is usually a copy, not a scroll, and it must not
/// make the browser's next move look like the reader's.
#[cfg(target_arch = "wasm32")]
fn attach_reader_gesture_listeners(pane: &web_sys::Element) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;

    for name in ["wheel", "touchmove", "keydown"] {
        let callback = Closure::<dyn FnMut(web_sys::Event)>::new(move |evt: web_sys::Event| {
            // Paging the transcript with the keyboard is the reader's move, too.
            // Every other key is not: typing a message must not release the pane.
            if name == "keydown"
                && !evt
                    .dyn_ref::<web_sys::KeyboardEvent>()
                    .is_some_and(|key| READER_KEYS.contains(&key.key().as_str()))
            {
                return;
            }
            READER_GESTURE_MS.with(|cell| cell.set(js_sys::Date::now()));
        });
        let _ = pane.add_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
        callback.forget();
    }
    // A scrollbar drag moves the mouse with a button held; a mouse that merely
    // passes over the pane does not scroll it.
    let drag = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(|evt: web_sys::MouseEvent| {
        if evt.buttons() != 0 {
            READER_GESTURE_MS.with(|cell| cell.set(js_sys::Date::now()));
        }
    });
    let _ = pane.add_event_listener_with_callback("mousemove", drag.as_ref().unchecked_ref());
    drag.forget();
}

/// Whether the scroll being handled was the reader's own.
#[cfg(target_arch = "wasm32")]
fn reader_scrolled() -> bool {
    let last = READER_GESTURE_MS.with(Cell::get);
    if last <= 0.0 {
        return false;
    }
    js_sys::Date::now() - last <= READER_GESTURE_WINDOW_MS
}

#[cfg(target_arch = "wasm32")]
fn note_pane_scroll(mut follow: Signal<TranscriptStick>) {
    let Some(pane) = messages_pane() else {
        return;
    };
    if viewport_shows_latest(
        pane.scroll_top(),
        pane.client_height(),
        pane.scroll_height(),
    ) {
        follow.write().note_at_bottom();
        return;
    }
    if reader_scrolled() {
        follow.write().note_reader_scroll();
        return;
    }
    // The browser moved the pane: the rows changed under it, and the reader did
    // not ask to leave the latest row. Put the pane back where the pin says.
    if follow.read().note_browser_scroll() == TranscriptJump::Bottom {
        let _ = apply_latest();
    }
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
fn messages_pane() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .get_element_by_id("chat-messages")
}

/// Apply a loaded history to the transcript, when it still belongs there.
///
/// A payload for a conversation the reader has left is stale: it would replace the
/// thread they are reading now with the one they came from. The reader's own
/// conversation owns the bottom of the pane; a refresh of it does not.
pub(super) fn apply_history(
    follow_signal: Signal<TranscriptStick>,
    messages_signal: Signal<Vec<ChatMsg>>,
    loaded_conv: &str,
    active: &Signal<Option<String>>,
    rows: Vec<ChatMsg>,
) -> bool {
    let mut follow = follow_signal;
    let mut messages = messages_signal;
    let Ok(current) = active.try_read() else {
        return false;
    };
    if current.as_deref() != Some(loaded_conv) {
        return false;
    }
    let _ = crate::signal_alive::with_mut_if_alive(&mut follow, |stick: &mut TranscriptStick| {
        stick.opened_conversation(loaded_conv)
    });
    let _ = crate::signal_alive::set_if_alive(&mut messages, rows);
    true
}

#[cfg(test)]
#[path = "chat_scroll_tests.rs"]
mod chat_scroll_tests;
