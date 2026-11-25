//! Pin and jump decisions for the transcript pane.
//!
//! The browser write is `scroll_top_to_apply` applied to `#chat-messages`.
//! These tests drive that decision. A DOM runner is not part of `cargo test`.

use super::{
    ChatMsg, LATEST_SLACK_PX, TranscriptJump, TranscriptStick, apply_history, is_reader_key,
    max_scroll_top, payload_owns_bottom, scroll_top_to_apply, viewport_shows_latest,
};

const CLIENT: i32 = 100;
const TALL: i32 = 800;

/// A reader who scrolled up into earlier messages.
fn away_from_latest() -> TranscriptStick {
    let mut stick = TranscriptStick::pinned();
    stick.note_reader_scroll();
    assert!(
        !viewport_shows_latest(0, CLIENT, TALL),
        "the fixture must start on earlier messages"
    );
    stick
}

/// Opening lands on the latest row after history arrives, even if the reader
/// had scrolled up. The row may be the human's or the agent's: both are the
/// bottom of the same pane.
#[test]
fn opening_lands_on_the_latest_row_from_either_side() {
    let mut stick = away_from_latest();
    stick.opened_conversation("conv-a");
    let bottom = max_scroll_top(CLIENT, TALL);
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, TALL),
        Some(bottom),
        "open must jump to the bottom edge"
    );
    assert!(viewport_shows_latest(bottom, CLIENT, TALL));
    assert_eq!(bottom, TALL - CLIENT);
}

/// The fetch returns after the pane is already showing. The pin from open is
/// what moves that later paint onto the latest row.
#[test]
fn history_that_arrives_after_open_still_jumps_to_the_bottom() {
    let mut stick = away_from_latest();
    assert_eq!(stick.on_content(), TranscriptJump::Hold);
    stick.opened_conversation("conv-a");
    // The rows are taller once the payload is in the pane. Scroll is still at the top.
    let loaded = TALL + 200;
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, loaded),
        Some(max_scroll_top(CLIENT, loaded))
    );
}

/// Sending jumps to the very bottom even when the reader had scrolled up.
#[test]
fn sending_jumps_to_the_bottom_after_the_reader_scrolled_up() {
    let mut stick = away_from_latest();
    stick.user_sent();
    assert_eq!(stick.on_content(), TranscriptJump::Bottom);
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, TALL),
        Some(max_scroll_top(CLIENT, TALL))
    );
}

/// Live growth holds still while the reader is up in earlier messages.
#[test]
fn growth_holds_when_the_reader_left_the_bottom() {
    let stick = away_from_latest();
    let grown = TALL + 40;
    assert_eq!(stick.on_content(), TranscriptJump::Hold);
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, grown),
        None
    );
}

/// A pane already on the latest row does not write `scrollTop` again.
#[test]
fn an_already_bottom_pane_is_left_alone() {
    let bottom = max_scroll_top(CLIENT, TALL);
    let stick = TranscriptStick::pinned();
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), bottom, CLIENT, TALL),
        None
    );
}

/// A short thread has no overflow. It already shows the latest row.
#[test]
fn a_short_transcript_already_shows_the_latest_row() {
    assert!(viewport_shows_latest(0, CLIENT, 40));
    assert_eq!(max_scroll_top(CLIENT, 40), 0);
    assert_eq!(
        scroll_top_to_apply(TranscriptJump::Bottom, 0, CLIENT, 40),
        None
    );
}

/// Slack absorbs a few pixels. One pixel past it is "scrolled up".
#[test]
fn slack_is_a_narrow_window_around_the_bottom_edge() {
    let bottom = max_scroll_top(CLIENT, TALL);
    let inside = bottom - LATEST_SLACK_PX;
    let outside = inside - 1;
    assert!(viewport_shows_latest(inside, CLIENT, TALL), "inside slack");
    assert!(!viewport_shows_latest(outside, CLIENT, TALL), "past slack");
    let mut stick = TranscriptStick::pinned();
    stick.note_reader_scroll();
    assert_eq!(stick.on_content(), TranscriptJump::Hold);
    stick.note_at_bottom();
    assert_eq!(stick.on_content(), TranscriptJump::Bottom);
}

/// The rows are replaced under the reader. The browser moves the pane on its
/// own — the content shrinks and `scrollTop` is clamped, or the content above
/// the viewport grows and the browser re-anchors it. The reader did not ask to
/// leave the latest row, so the open must still land there.
///
/// This is the failure that left a thread on its oldest row: the move dropped
/// the pin, and the pane stayed where the browser put it.
#[test]
fn a_browser_move_does_not_drop_the_open_pin() {
    let mut stick = away_from_latest();
    stick.opened_conversation("conv-a");
    // The browser moved the pane while the rows were replaced.
    assert_eq!(
        stick.note_browser_scroll(),
        TranscriptJump::Bottom,
        "the open pin must survive a move the browser made"
    );
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, TALL),
        Some(max_scroll_top(CLIENT, TALL)),
        "and the pane must go back to the bottom"
    );
}

/// The same for a send: the line the reader just sent is the bottom of the
/// thread, and a browser move must not leave it below the fold.
#[test]
fn a_browser_move_does_not_drop_the_send_pin() {
    let mut stick = away_from_latest();
    stick.user_sent();
    assert_eq!(stick.note_browser_scroll(), TranscriptJump::Bottom);
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, TALL),
        Some(max_scroll_top(CLIENT, TALL))
    );
}

/// A browser move must not drag a reader who scrolled up back to the bottom
/// either: they keep the place they chose.
#[test]
fn a_browser_move_leaves_a_reader_who_scrolled_up_alone() {
    let mut stick = TranscriptStick::pinned();
    stick.note_reader_scroll();
    assert_eq!(
        stick.note_browser_scroll(),
        TranscriptJump::Hold,
        "the reader's place stands against a move the browser made"
    );
}

/// The reader's own scroll still releases the pane. A reply must not pull them
/// back down while they read an earlier message.
#[test]
fn a_reader_scroll_still_releases_the_pane() {
    let mut stick = TranscriptStick::pinned();
    stick.note_reader_scroll();
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "a reader who scrolled up keeps that place while the reply grows"
    );
    assert_eq!(
        scroll_top_to_apply(stick.on_content(), 0, CLIENT, TALL + 40),
        None
    );
}

/// A reader scroll after a browser move still wins: the browser's move is not
/// the reader's, and only the reader releases the pane.
#[test]
fn a_reader_scroll_after_a_browser_move_still_releases_the_pane() {
    let mut stick = TranscriptStick::pinned();
    assert_eq!(stick.note_browser_scroll(), TranscriptJump::Bottom);
    stick.note_reader_scroll();
    assert_eq!(stick.on_content(), TranscriptJump::Hold);
}

/// The refresh at the end of the reader's own turn must not take the bottom back.
///
/// The reader sent, the reply streamed, and they scrolled up into an earlier
/// message. The turn ends, the history is fetched again for the same
/// conversation, and that payload is a refresh of the thread the reader is in —
/// not a conversation they just opened. The pane must stay where they put it.
#[test]
fn the_refresh_at_the_end_of_a_turn_does_not_re_pin_a_reader_who_scrolled_up() {
    let mut stick = TranscriptStick::pinned();
    // The reader sent. The conversation their send opens is the one on screen.
    stick.user_sent();
    // The reply streams, and the reader scrolls up into an earlier message.
    stick.note_reader_scroll();
    // The turn ends: the history is fetched again for the same conversation.
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "the end-of-turn refresh must not take the bottom back"
    );
}

/// The stream reports the conversation the reader's send opened, so the pane is
/// showing it. The refresh at the end of that turn is still a refresh of the
/// thread the reader is in, and must not take the bottom back.
#[test]
fn the_conversation_the_readers_send_opened_is_not_a_new_open() {
    let mut stick = TranscriptStick::pinned();
    stick.user_sent();
    // The stream reports the conversation the send opened.
    stick.note_sent_conversation("conv-a");
    // The reader scrolls up while the reply streams.
    stick.note_reader_scroll();
    // The turn ends: the history is fetched again for the same conversation.
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "the end-of-turn refresh must not take the bottom back"
    );
}

/// A conversation the reader picked is not the one their send opened. It owns the
/// bottom of the pane, even if they had scrolled up in the thread they came from.
#[test]
fn a_conversation_the_reader_picked_still_takes_the_bottom() {
    let mut stick = TranscriptStick::pinned();
    stick.user_sent();
    stick.note_sent_conversation("conv-a");
    // The reader had scrolled up in the thread they came from.
    stick.note_reader_scroll();
    // The reader picked another conversation while the turn was running.
    stick.note_shown_conversation(Some("conv-b"));
    stick.opened_conversation("conv-b");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Bottom,
        "the conversation the reader picked owns the bottom of the pane"
    );
}

/// The whole end-of-turn sequence, as the live path runs it.
///
/// The reader opens a new chat, sends, the stream reports the conversation the
/// send opened, the reply streams and the reader scrolls up into an earlier
/// message. The turn ends and the history is fetched again for that same
/// conversation. The pane must stay where the reader put it.
#[test]
fn the_live_end_of_turn_sequence_keeps_the_readers_place() {
    let mut stick = TranscriptStick::pinned();
    // A new chat: no conversation is on screen yet.
    stick.note_shown_conversation(None);
    // The reader sends, and the stream reports the conversation the send opened.
    stick.user_sent();
    stick.note_sent_conversation("conv-a");
    // The reply streams, and the reader scrolls up into an earlier message.
    stick.note_reader_scroll();
    // The turn ends. The pane still shows that conversation, and the history is
    // fetched again for it.
    stick.note_shown_conversation(Some("conv-a"));
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "the end-of-turn refresh must not take the bottom back"
    );
}

/// The same sequence in a conversation the reader opened. The open owns the
/// bottom; the refresh at the end of the turn does not.
#[test]
fn the_live_end_of_turn_sequence_keeps_the_readers_place_in_an_open_chat() {
    let mut stick = TranscriptStick::pinned();
    // The reader opens the conversation, and the history lands.
    stick.note_shown_conversation(Some("conv-a"));
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Bottom,
        "the conversation the reader opened owns the bottom"
    );
    // The reader sends, and the stream reports the conversation the send opened.
    stick.user_sent();
    stick.note_sent_conversation("conv-a");
    // The reply streams, and the reader scrolls up into an earlier message.
    stick.note_reader_scroll();
    // The turn ends, and the history is fetched again for the same conversation.
    stick.note_shown_conversation(Some("conv-a"));
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "the end-of-turn refresh must not take the bottom back"
    );
}

/// The stream has not reported the conversation yet, and the pane was showing
/// nothing. The conversation that appears is the one the reader's send opened, so
/// the refresh is still a refresh of their own turn.
#[test]
fn the_refresh_for_the_sent_conversation_is_a_refresh_before_the_stream_reports_it() {
    let mut stick = TranscriptStick::pinned();
    stick.user_sent();
    // The reply streams, and the reader scrolls up into an earlier message.
    stick.note_reader_scroll();
    // The turn ends. The pane was showing nothing, so the conversation that
    // appears is the one the send opened.
    stick.note_shown_conversation(Some("conv-a"));
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "the end-of-turn refresh must not take the bottom back"
    );
}

/// The keys that page the transcript are the reader's own move, so they release
/// the pane the same way a wheel or a touch does.
#[test]
fn the_keys_that_page_the_transcript_are_reader_gestures() {
    for key in ["PageUp", "PageDown", "Home", "End", "ArrowUp", "ArrowDown"] {
        assert!(is_reader_key(key), "{key} moves the transcript");
    }
    for key in ["a", "Enter", "Tab", "Escape", "Shift"] {
        assert!(!is_reader_key(key), "{key} does not move the transcript");
    }
}

/// A conversation the reader just opened owns the bottom of the pane.
#[test]
fn a_new_conversation_takes_the_bottom() {
    assert!(payload_owns_bottom("conv-b", Some("conv-a")));
    assert!(payload_owns_bottom("conv-b", None));
}
/// A refresh of the conversation already on screen does not. The reader may
/// have scrolled up while the reply was streaming, and the refresh must not
/// drag them back down.
#[test]
fn a_refresh_of_the_open_conversation_does_not_take_the_bottom() {
    assert!(!payload_owns_bottom("conv-a", Some("conv-a")));
}

/// The stick applies that rule: opening a conversation pins the pane, and the
/// refresh that follows the same conversation leaves the reader's place alone.
#[test]
fn a_refresh_does_not_re_pin_a_reader_who_scrolled_up() {
    let mut stick = TranscriptStick::pinned();
    stick.opened_conversation("conv-a");
    // The reader scrolled up while the reply streamed.
    stick.note_reader_scroll();
    // The history is fetched again when the turn ends. Same conversation.
    stick.opened_conversation("conv-a");
    assert_eq!(
        stick.on_content(),
        TranscriptJump::Hold,
        "the refresh must not take the bottom back"
    );
    // Switching to another conversation takes it back.
    stick.opened_conversation("conv-b");
    assert_eq!(stick.on_content(), TranscriptJump::Bottom);
}

/// The history fetch is asynchronous, so it can land after the reader has moved on.
///
/// A payload for the conversation the reader left is stale: it would replace the
/// thread they are reading now with the one they came from, and the pane would
/// follow the wrong thread to its bottom.
#[cfg(test)]
mod history_apply {
    use super::*;
    use dioxus::prelude::*;
    use std::cell::RefCell;

    struct Held {
        follow: Signal<TranscriptStick>,
        messages: Signal<Vec<ChatMsg>>,
        active: Signal<Option<String>>,
    }

    thread_local! {
        static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
    }

    #[component]
    fn Keeper() -> Element {
        let follow = use_signal(TranscriptStick::pinned);
        let messages = use_signal(Vec::<ChatMsg>::new);
        let active = use_signal(|| Some("conv-b".to_string()));
        HELD.with(|cell| {
            *cell.borrow_mut() = Some(Held {
                follow,
                messages,
                active,
            })
        });
        rsx! {}
    }

    fn row(text: &str) -> ChatMsg {
        ChatMsg {
            role: "user",
            content: text.to_string(),
            thinking_steps: Vec::new(),
            reasoning: None,
        }
    }

    /// The signals live in the rendered scope, so the dom has to outlive them.
    fn held_signals() -> (dioxus_core::VirtualDom, Held) {
        let mut dom = dioxus_core::VirtualDom::new(|| rsx! { Keeper {} });
        dom.rebuild_in_place();
        let held = HELD
            .with(|cell| cell.borrow_mut().take())
            .expect("the signals");
        (dom, held)
    }

    #[test]
    fn a_payload_for_the_conversation_the_reader_left_is_dropped() {
        let (_dom, held) = held_signals();
        let applied = apply_history(
            held.follow,
            held.messages,
            "conv-a",
            &held.active,
            vec![row("from the thread they left")],
        );
        assert!(!applied, "a stale payload must not be applied");
        assert!(
            held.messages.read().is_empty(),
            "the thread on screen must stay the one the reader is reading"
        );
    }

    #[test]
    fn the_payload_for_the_conversation_on_screen_lands_and_pins_the_pane() {
        let (_dom, held) = held_signals();
        let applied = apply_history(
            held.follow,
            held.messages,
            "conv-b",
            &held.active,
            vec![row("the thread they opened")],
        );
        assert!(applied, "the payload on screen must be applied");
        assert_eq!(held.messages.read().len(), 1);
        assert_eq!(
            held.follow.read().on_content(),
            TranscriptJump::Bottom,
            "the opened conversation owns the bottom of the pane"
        );
    }
}
