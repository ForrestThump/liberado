//! Pin and jump decisions for the transcript pane.
//!
//! The browser write is `scroll_top_to_apply` applied to `#chat-messages`.
//! These tests drive that decision. A DOM runner is not part of `cargo test`.

use super::{
    LATEST_SLACK_PX, TranscriptJump, TranscriptStick, max_scroll_top, scroll_top_to_apply,
    viewport_shows_latest,
};

const CLIENT: i32 = 100;
const TALL: i32 = 800;

fn away_from_latest() -> TranscriptStick {
    let mut stick = TranscriptStick::pinned();
    stick.note_viewport(0, CLIENT, TALL);
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
    stick.opened_conversation();
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
    stick.opened_conversation();
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
    stick.note_viewport(outside, CLIENT, TALL);
    assert_eq!(stick.on_content(), TranscriptJump::Hold);
    stick.note_viewport(inside, CLIENT, TALL);
    assert_eq!(stick.on_content(), TranscriptJump::Bottom);
}
