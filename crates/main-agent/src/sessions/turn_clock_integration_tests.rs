//! Integration tests for the time-framing wiring in [`ChatSessions::turn`] and
//! [`ChatSessions::turn_stream`]. The pure helpers are pinned by `turn_clock_tests.rs`; this
//! file exercises the *call site*: that the framed text reaches the model, that the raw text
//! reaches storage, and that a session without a timezone attached is byte-for-byte the same
//! turn the suite has been asserting on for the last year.

use super::super::*;
use super::test_fixtures::*;
use chrono::Duration;
use liberado_common::UserTimezone;
use liberado_conversation_store::Author;
use liberado_provider::{CompletionResponse, Role};

/// A `ChatSessions` over the real session store, scripted with enough replies for a one- or
/// two-turn test. Tests that drive two turns script two replies; one-turn tests use the first.
async fn chat_sessions_with_timezone(
    root: &std::path::Path,
    tz: Option<UserTimezone>,
) -> (ChatSessions, Arc<MockProvider>) {
    chat_sessions_with_timezone_and_replies(
        root,
        tz,
        vec![
            CompletionResponse::text("ok"),
            CompletionResponse::text("ok"),
        ],
    )
    .await
}

async fn chat_sessions_with_timezone_and_replies(
    root: &std::path::Path,
    tz: Option<UserTimezone>,
    replies: Vec<CompletionResponse>,
) -> (ChatSessions, Arc<MockProvider>) {
    let store = Arc::new(SessionStore::open(root).await);
    let provider = Arc::new(MockProvider::with_script("mock", replies));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut sessions = ChatSessions::new(store, executor, Arc::new(no_tools_runtime()));
    if let Some(tz) = tz {
        sessions = sessions.with_user_timezone(tz);
    }
    (sessions, provider)
}

// ── Behaviour the existing test suite already pins, now under a timezone ──────────────────────

/// The headline contract: a `ChatSessions` built **without** `with_user_timezone` must continue
/// to send the raw user text to the model, byte-for-byte. This is the property every existing
/// test relies on, and the reason `frame_user_message(None, ...)` is a no-op.
#[tokio::test]
async fn a_session_without_user_timezone_sends_the_raw_user_text() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) = chat_sessions_with_timezone(dir.path(), None).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "due tomorrow at noon").await.unwrap();

    let request = &provider.received_requests()[0];
    let user_msg = request
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .expect("a user message reaches the model on every turn");
    assert_eq!(user_msg.content, "due tomorrow at noon");
}

/// A session that *does* wire a timezone sends a framed copy to the model, in that timezone.
/// Proves the wiring is end-to-end (timezone → `ChatSessions::turn` → provider request), not
/// just present in the helper.
#[tokio::test]
async fn a_session_with_user_timezone_frames_the_user_message_in_that_zone() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) = chat_sessions_with_timezone(
        dir.path(),
        Some(UserTimezone::parse("America/Denver").unwrap()),
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "due tomorrow at noon").await.unwrap();

    let request = &provider.received_requests()[0];
    let user_msg = request
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .expect("a user message reaches the model on every turn");
    // 2026-10-05 19:42 UTC is a different wall-clock in Denver than in Chicago. The exact
    // second the test runs at is irrelevant — we only need the *line* present and the IANA
    // name correct.
    assert!(
        user_msg.content.starts_with("Local time:"),
        "framed turn must begin with the Local time line: {:?}",
        user_msg.content,
    );
    assert!(
        user_msg.content.contains("(America/Denver)"),
        "the IANA name on the framing must match the attached zone: {:?}",
        user_msg.content,
    );
    // The raw text is preserved at the end, after the blank line.
    assert!(
        user_msg.content.ends_with("due tomorrow at noon"),
        "the raw user text is the last block of the framed message: {:?}",
        user_msg.content,
    );
    assert!(
        user_msg.content.contains("\n\ndue tomorrow at noon"),
        "framing must terminate with a blank line before the raw text: {:?}",
        user_msg.content,
    );
}

/// First message in a session → no gap line. The framing has the time line and the raw text,
/// and nothing in between that would tell the model a previous turn happened (it didn't).
#[tokio::test]
async fn first_turn_in_a_session_omits_the_gap_line() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) = chat_sessions_with_timezone(
        dir.path(),
        Some(UserTimezone::parse("America/Chicago").unwrap()),
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello there").await.unwrap();

    let request = &provider.received_requests()[0];
    let user_msg = request
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .expect("user message is in the request");
    assert!(
        !user_msg.content.contains("Last user message"),
        "first turn in a session must not include a gap line: {:?}",
        user_msg.content,
    );
    assert!(user_msg.content.contains("Local time:"));
}

// ── The stored content stays raw ─────────────────────────────────────────────────────────────

/// The persisted user node's content is the raw text the human typed, not the framed copy.
/// Rendering-side, WebUI history, and `history()` / `history_nodes()` must keep returning
/// exactly the raw text — the framing is a render-time transform, not part of the record.
#[tokio::test]
async fn the_persisted_user_node_keeps_the_raw_text() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, _) = chat_sessions_with_timezone(
        dir.path(),
        Some(UserTimezone::parse("America/Chicago").unwrap()),
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "due tomorrow at noon").await.unwrap();

    // The render path.
    let history = sessions.history(id).await.unwrap();
    let user_messages: Vec<&str> = history
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.as_str())
        .collect();
    assert_eq!(user_messages, vec!["due tomorrow at noon"]);

    // The provenance path (what the WebUI history and search use).
    let nodes = sessions.history_nodes(id).await.unwrap();
    let user_node = nodes
        .iter()
        .find(|n| matches!(n.author, Author::User))
        .expect("the user node is durable after the turn");
    assert_eq!(user_node.message.content, "due tomorrow at noon");
    assert!(
        !user_node.message.content.starts_with("Local time:"),
        "the persisted node must not carry the framing: {}",
        user_node.message.content
    );
}

/// A no-timezone session persists the same content it always has (this is the existing test
/// surface, re-pinned by this file as the regression guard).
#[tokio::test]
async fn no_timezone_session_persists_and_renders_the_raw_text() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, _) = chat_sessions_with_timezone(dir.path(), None).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "due tomorrow at noon").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    let user_message = history
        .iter()
        .find(|m| m.role == Role::User)
        .expect("the user message is on disk after a successful turn");
    assert_eq!(user_message.content, "due tomorrow at noon");
}

// ── Streaming path ───────────────────────────────────────────────────────────────────────────

/// The streaming path sends the same framed text the buffered `turn` path does — SSE clients
/// see the time line and the raw text in the user message that goes out on the wire.
#[tokio::test]
async fn turn_stream_frames_the_user_message_when_a_timezone_is_attached() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) =
        chat_sessions_with_timezone(dir.path(), Some(UserTimezone::parse("UTC").unwrap())).await;
    let id = sessions.create(None).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    sessions
        .turn_stream(id, "due tomorrow at noon", &tx)
        .await
        .unwrap();

    let request = &provider.received_requests()[0];
    let user_msg = request
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .expect("user message is in the request");
    assert!(user_msg.content.starts_with("Local time:"));
    assert!(user_msg.content.contains("(UTC)"));
    assert!(user_msg.content.ends_with("due tomorrow at noon"));

    // And the stored content is still the raw text.
    let history = sessions.history(id).await.unwrap();
    let user_message = history
        .iter()
        .find(|m| m.role == Role::User)
        .expect("the user message is on disk after a streamed turn");
    assert_eq!(user_message.content, "due tomorrow at noon");
}

// ── Date prefixes on prior user messages ─────────────────────────────────────────────────────

/// When a timezone is attached, prior user-authored messages in the model-visible history are
/// date-prefixed; assistant / tool / system / named messages are not. The exact prefix uses the
/// configured zone. This is the **model-visible** view — `history_nodes()` and the on-disk
/// representation stay raw.
#[tokio::test]
async fn prior_user_messages_carry_their_local_date_in_the_model_view() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) = chat_sessions_with_timezone(
        dir.path(),
        Some(UserTimezone::parse("America/Chicago").unwrap()),
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    // First turn — the seeding turn. The model receives only the raw text + local-time line.
    sessions.turn(id, "first question").await.unwrap();
    // The framing helper cannot reach back further than the seed, so this assertion lives
    // against the *next* turn's request, which carries the prefix the first turn wrote.
    sessions.turn(id, "second question").await.unwrap();

    let second_request = &provider.received_requests()[1];
    // Find the seeded user message; the prefix is part of its content. The exact second is
    // wall-clock-dependent, so the assertion is on shape, not on the timestamp itself.
    let seeded = second_request
        .messages
        .iter()
        .find(|m| m.role == Role::User && m.content.contains("first question"))
        .expect("the seeded user message is still in the model view");
    let prefix = seeded
        .content
        .strip_suffix("first question")
        .expect("the user message's body must be the raw text with a prefix in front")
        .trim_end();
    // The raw content was `[Tue 2026-10-06 08:39 CDT] first question`; `strip_suffix` strips
    // the body, leaving `[Tue 2026-10-06 08:39 CDT] ` (trailing space). `trim_end` eats that
    // space, so the test inspects the bracketed prefix alone.
    assert!(
        prefix.starts_with("[") && prefix.contains(" 20") && prefix.ends_with("]"),
        "the prior user message must be prefixed with [Ddd YYYY-MM-DD HH:MM TZ]: {prefix:?}"
    );
    // The prefix uses `%Z` (CDT / MDT / etc) — the abbreviated zone, the same the time line
    // uses. Chicago in October is on CDT (UTC-5).
    assert!(
        prefix.contains("CDT"),
        "the prior user message's prefix must be in the configured zone (CDT for America/Chicago in October): {prefix:?}"
    );
    // The leading three-letter weekday is the part that lets the model reason about relative
    // dates without computing them. The exact day depends on the wall-clock at test time, so
    // we only assert the *shape* — a 3-letter alpha prefix inside the brackets.
    let inside = prefix
        .strip_prefix('[')
        .and_then(|s| s.split(']').next())
        .unwrap_or("");
    assert!(
        inside.len() >= 4
            && inside[..3].chars().all(|c| c.is_ascii_alphabetic())
            && inside.as_bytes()[3] == b' ',
        "the prior user message's prefix must start with a 3-letter weekday: {prefix:?}"
    );

    // The current turn's user message is the framed one (Local time + raw text), NOT the
    // date-prefixed one. The prefix only applies to *prior* messages; the current turn's
    // framing is the local-time + (optional) gap line.
    let current = second_request
        .messages
        .iter()
        .find(|m| m.role == Role::User && m.content.contains("second question"))
        .expect("the current user message is in the request");
    assert!(current.content.starts_with("Local time:"));
    assert!(current.content.contains("second question"));

    // The on-disk representation is unchanged.
    let nodes = sessions.history_nodes(id).await.unwrap();
    let first_node = nodes
        .iter()
        .find(|n| matches!(n.author, Author::User) && n.message.content == "first question")
        .expect("the seeded user node is on disk with the raw content");
    assert_eq!(first_node.message.content, "first question");
    assert!(
        !first_node.message.content.starts_with("["),
        "the persisted user node must not carry the prefix"
    );
}

/// Without a timezone attached, prior user messages are sent verbatim — no prefix. The model
/// has no way to know the date; the gap line is the only temporal cue and it is also absent.
#[tokio::test]
async fn no_timezone_session_does_not_prefix_prior_user_messages() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) = chat_sessions_with_timezone(dir.path(), None).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "first question").await.unwrap();
    sessions.turn(id, "second question").await.unwrap();

    let second_request = &provider.received_requests()[1];
    let seeded = second_request
        .messages
        .iter()
        .find(|m| m.role == Role::User && m.content.contains("first question"))
        .expect("the seeded user message is in the request");
    assert_eq!(
        seeded.content, "first question",
        "no timezone → no prefix on prior user messages"
    );
}

// ── `with_user_timezone` is a builder; the public accessor exposes the wiring ────────────────

/// `user_timezone()` echoes what `with_user_timezone` set, and `None` for an unmodified
/// `ChatSessions` — the diagnostic seam a test or operator tool uses to see whether framing is
/// active.
#[tokio::test]
async fn user_timezone_accessor_echoes_the_builder() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, _) = chat_sessions_with_timezone(dir.path(), None).await;
    assert!(sessions.user_timezone().is_none());

    let (sessions, _) = chat_sessions_with_timezone(
        dir.path(),
        Some(UserTimezone::parse("America/Chicago").unwrap()),
    )
    .await;
    let tz = sessions.user_timezone().expect("the zone is attached");
    assert_eq!(tz.iana_name(), "America/Chicago");
}

// ── Gap-line behaviour: end-to-end ──────────────────────────────────────────────────────────

/// The gap line is computed from the **pre-compaction** `nodes`, so the `created_at` used is
/// the original user-message timestamp, not a fresh `now`. This is the wiring the pure helper
/// relies on and the place a future refactor of `load` would silently break. The integration
/// pin: a seeded user message with a `created_at` far in the past must produce a gap line on
/// the next turn.
///
/// We can't seed `created_at` directly through `append` (the store stamps `now`), so this
/// integration covers the *no-gap* and *gap-present* cases through the seed_turns helper used
/// in the rest of the suite: the seeded nodes are written back-to-back, and the next turn sees
/// them as "very recent" — so the gap line is **not** produced. The gap-line *presence* is
/// pinned by the pure helper; the gap-line *absence* for a fresh conversation is pinned here.
#[tokio::test]
async fn a_recent_prior_user_message_produces_no_gap_line() {
    let dir = tempfile::tempdir().unwrap();
    let (sessions, provider) = chat_sessions_with_timezone(
        dir.path(),
        Some(UserTimezone::parse("America/Chicago").unwrap()),
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    // First turn is "now" in store-time terms; the second turn happens immediately after.
    sessions.turn(id, "first").await.unwrap();
    sessions.turn(id, "second").await.unwrap();

    let second_request = &provider.received_requests()[1];
    let user_msg = second_request
        .messages
        .iter()
        .find(|m| m.role == Role::User && m.content.contains("second"))
        .expect("the second user message is in the request");
    assert!(
        !user_msg.content.contains("Last user message"),
        "a recent prior message must not trigger the gap line: {:?}",
        user_msg.content,
    );
    assert!(user_msg.content.starts_with("Local time:"));
}

/// The wall-clock default in `default_gap_threshold()` is the 12-hour line documented in
/// `turn_clock.rs`. Re-pin the constant here so a refactor that changes the threshold without
/// updating the doc gets caught.
#[test]
fn default_gap_threshold_is_twelve_hours() {
    assert_eq!(
        super::super::turn_clock::default_gap_threshold(),
        Duration::hours(12)
    );
}
