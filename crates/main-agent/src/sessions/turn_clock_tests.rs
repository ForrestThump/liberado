//! Pure-function tests for [`super`] — the time-framing, gap note, and date-prefix logic
//! that [`ChatSessions`](super::super::ChatSessions) hands to the model. No I/O, no
//! `ChatSessions`; pinning these by themselves catches regressions without spinning up a store
//! and a provider.

use super::*;
use chrono::TimeZone;
use liberado_conversation_store::{Author, Ulid};
use liberado_provider::{Message, Role};

fn chicago() -> UserTimezone {
    UserTimezone::parse("America/Chicago").unwrap()
}

fn denver() -> UserTimezone {
    UserTimezone::parse("America/Denver").unwrap()
}

fn utc() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 5, 19, 42, 0).unwrap()
}

/// Build a `MessageNode` for tests. ULID details are irrelevant — the helpers under test only
/// read `author` and `created_at` — so we mint fresh ids.
fn node(_index: usize, author: Author, content: &str, created_at: DateTime<Utc>) -> MessageNode {
    MessageNode {
        id: Ulid::new(),
        parent_id: None,
        conversation_id: Ulid::new(),
        author: author.clone(),
        created_at,
        message: Message {
            role: role_for(&author),
            content: content.to_string(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        model: None,
        reasoning: None,
    }
}

fn role_for(author: &Author) -> Role {
    match author {
        Author::System => Role::System,
        Author::User => Role::User,
        Author::Assistant => Role::Assistant,
        Author::Tool => Role::Tool,
        Author::Named(_) => Role::Assistant,
    }
}

#[test]
fn frame_user_message_without_timezone_is_a_no_op() {
    // The headline guarantee: a session built without `with_user_timezone` continues to see the
    // raw text, byte-for-byte. Existing tests rely on it; this test pins the property where the
    // helper lives.
    let now = utc();
    assert_eq!(
        frame_user_message(
            None,
            now,
            None,
            default_gap_threshold(),
            "due tomorrow at noon"
        ),
        "due tomorrow at noon"
    );
}

#[test]
fn frame_user_message_with_timezone_includes_the_local_time_line() {
    // 2026-10-05 19:42 UTC → 2026-10-05 14:42 CDT (Chicago, UTC-5 in October).
    let now = utc();
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        None,
        default_gap_threshold(),
        "due tomorrow at noon",
    );
    assert!(framed.starts_with("Local time: 2026-10-05 14:42 CDT (America/Chicago)."));
    assert!(framed.contains("(America/Chicago)"));
    // Raw text is preserved, verbatim and last (the model reads from the most recent
    // instruction downward).
    assert!(framed.ends_with("due tomorrow at noon"));
    assert!(framed.contains("\n\ndue tomorrow at noon"));
}

#[test]
fn frame_user_message_uses_the_configured_zone_not_chicago() {
    // A different timezone must produce a different line — same UTC, different wall clock and
    // different IANA name. Proves the helper does not bake a default in.
    let now = utc();
    let framed = frame_user_message(
        Some(&denver()),
        now,
        None,
        default_gap_threshold(),
        "due tomorrow at noon",
    );
    // Denver is UTC-6 in October (MDT), 13:42 local.
    assert!(framed.starts_with("Local time: 2026-10-05 13:42 MDT (America/Denver)."));
    assert!(!framed.contains("America/Chicago"));
    assert!(!framed.contains("CDT"));
}

#[test]
fn gap_line_is_omitted_just_under_threshold() {
    // 12h threshold: 11h 59m → no line. The 12h rounding is part of the contract.
    let now = utc();
    let prev = now - chrono::Duration::hours(11) - chrono::Duration::minutes(59);
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        Some(prev),
        default_gap_threshold(),
        "hi",
    );
    assert!(!framed.contains("Last user message"));
    // Still framed, though — the time line is unconditional once a timezone is attached.
    assert!(framed.starts_with("Local time:"));
}

#[test]
fn gap_line_is_omitted_at_exactly_the_threshold() {
    // The threshold is *strict* — a gap of exactly 12h does not trigger the line. A `> delta`
    // check would still be correct for the 11h 59m case above; the boundary test pins the
    // exact inequality, which a `<` mutation would silently flip.
    let now = utc();
    let prev = now - chrono::Duration::hours(12);
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        Some(prev),
        default_gap_threshold(),
        "hi",
    );
    assert!(
        !framed.contains("Last user message"),
        "a gap of exactly the threshold must not produce a gap line: {framed}"
    );
    assert!(framed.starts_with("Local time:"));
}

#[test]
fn gap_line_appears_just_over_threshold() {
    // 12h 1m → "12 hours 1 minute" gap line (the minutes are part of what makes it strictly
    // over the 12h threshold, so they show).
    let now = utc();
    let prev = now - chrono::Duration::hours(12) - chrono::Duration::minutes(1);
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        Some(prev),
        default_gap_threshold(),
        "hi",
    );
    assert!(framed.contains("Last user message was 12 hours 1 minute ago"));
    // The previous message's local time is part of the line (the model can ground a "when" in
    // the absolute date, not just the relative gap). 19:42 UTC − 12h01 = 07:41 UTC = 02:41 CDT
    // (Chicago is UTC-5 in October).
    assert!(
        framed.contains("[2026-10-05 02:41 CDT]"),
        "the previous message's local timestamp must appear in the gap line: {framed}"
    );
}

#[test]
fn gap_line_humanizes_multi_day_gaps() {
    // 2026-10-03 13:36 CDT = 2026-10-03 18:36 UTC. 2026-10-05 19:42 UTC is 2d 1h 6m later.
    let now = utc();
    let prev = Utc.with_ymd_and_hms(2026, 10, 3, 18, 36, 0).unwrap();
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        Some(prev),
        default_gap_threshold(),
        "due tomorrow at noon",
    );
    assert!(framed.contains("2 days 1 hour ago"));
    assert!(framed.contains("[2026-10-03 13:36 CDT]"));
}

#[test]
fn gap_line_uses_days_and_hours_for_two_day_gap() {
    // Exactly 2d 6h after the previous message.
    let now = utc();
    let prev = now - chrono::Duration::days(2) - chrono::Duration::hours(6);
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        Some(prev),
        default_gap_threshold(),
        "hi",
    );
    assert!(framed.contains("Last user message was 2 days 6 hours ago"));
}

#[test]
fn gap_line_is_singular_at_one_day_one_hour() {
    // "1 day 1 hour" reads more honestly than "1 days 1 hours" — the pluralization rule.
    let now = utc();
    let prev = now - chrono::Duration::days(1) - chrono::Duration::hours(1);
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        Some(prev),
        default_gap_threshold(),
        "hi",
    );
    assert!(framed.contains("1 day 1 hour ago"));
    assert!(!framed.contains("1 days"));
    assert!(!framed.contains("1 hours"));
}

#[test]
fn no_gap_line_on_first_message_in_a_session() {
    // First turn: no previous user message → no gap line. The model can still see the local time
    // line, but the framing for the gap is silently absent.
    let now = utc();
    let framed = frame_user_message(
        Some(&chicago()),
        now,
        None,
        default_gap_threshold(),
        "hello there",
    );
    assert!(!framed.contains("Last user message"));
    assert!(framed.starts_with("Local time:"));
    assert!(framed.ends_with("hello there"));
}

#[test]
fn humanize_gap_unit_boundaries() {
    // Sub-minute → "less than a minute". Re-runs of the same second must not change.
    assert_eq!(
        humanize_gap(chrono::Duration::seconds(0)),
        "less than a minute"
    );
    assert_eq!(
        humanize_gap(chrono::Duration::seconds(59)),
        "less than a minute"
    );
    // Minutes only.
    assert_eq!(humanize_gap(chrono::Duration::minutes(1)), "1 minute");
    assert_eq!(humanize_gap(chrono::Duration::minutes(5)), "5 minutes");
    // Hours only.
    assert_eq!(humanize_gap(chrono::Duration::hours(1)), "1 hour");
    assert_eq!(humanize_gap(chrono::Duration::hours(5)), "5 hours");
    // Hours + minutes (so a 1h 5m gap does not round to "1 hour" and lose the minutes that
    // proved the gap is more than an hour).
    assert_eq!(
        humanize_gap(chrono::Duration::minutes(65)),
        "1 hour 5 minutes"
    );
    // Day + hour.
    assert_eq!(humanize_gap(chrono::Duration::days(1)), "1 day");
    assert_eq!(
        humanize_gap(chrono::Duration::days(2) + chrono::Duration::hours(6)),
        "2 days 6 hours"
    );
    // Day-only at scale.
    assert_eq!(humanize_gap(chrono::Duration::days(5)), "5 days");
    // Negative durations are clamped to zero (defensive — `compute_gap_line` filters < threshold
    // before reaching here, but the helper alone is callable).
    assert_eq!(
        humanize_gap(chrono::Duration::seconds(-30)),
        "less than a minute"
    );
}

#[test]
fn previous_user_created_at_ignores_compaction_tail_copies() {
    // A tail copy's `created_at` is the compaction time, not the typing time. A naive
    // `nodes.iter().rev().find(User).created_at` would mark every long chat "just now" forever;
    // the helper must skip the named tail author.
    let now = utc();
    let older_user = now - chrono::Duration::days(2);
    let nodes = vec![
        node(1, Author::User, "two days ago", older_user),
        node(2, Author::Assistant, "ack", now - chrono::Duration::days(2)),
        node(
            3,
            Author::Named("compaction".into()),
            "summary",
            now - chrono::Duration::hours(1),
        ),
        node(
            4,
            Author::Named("compaction-tail".into()),
            "two days ago",
            now,
        ),
    ];
    let prev = previous_user_created_at(&nodes);
    assert_eq!(
        prev,
        Some(older_user),
        "tail copy must not be the latest 'User'"
    );
}

#[test]
fn previous_user_created_at_returns_none_for_empty_or_assistant_only() {
    // No user message at all → None. The framing helper then produces no gap line.
    assert!(previous_user_created_at(&[]).is_none());
    let assistant_only = vec![node(1, Author::Assistant, "I'm here to help", utc())];
    assert!(previous_user_created_at(&assistant_only).is_none());
}

#[test]
fn previous_user_created_at_finds_the_most_recent_user() {
    let t0 = utc() - chrono::Duration::days(10);
    let t1 = utc() - chrono::Duration::days(5);
    let t2 = utc() - chrono::Duration::days(2);
    let nodes = vec![
        node(1, Author::User, "first", t0),
        node(2, Author::Assistant, "reply", t0),
        node(3, Author::User, "second", t1),
        node(4, Author::Assistant, "reply", t1),
        node(5, Author::User, "third", t2),
    ];
    assert_eq!(previous_user_created_at(&nodes), Some(t2));
}

#[test]
fn prefix_user_message_dates_only_prefixes_user_messages() {
    // The rule: only `Author::User` is dated, in the user's timezone. A tool result, an assistant
    // reply, a profile-switch note, the system root, and a compaction marker all pass through
    // untouched.
    let now = utc();
    let user_at = now - chrono::Duration::days(1);
    let nodes = vec![
        node(1, Author::System, "system prompt", now),
        node(2, Author::User, "yesterday", user_at),
        node(3, Author::Assistant, "ok", user_at),
        node(4, Author::Tool, "tool result", user_at),
        node(5, Author::Named("profile".into()), "switched", user_at),
        node(6, Author::User, "second question", now),
    ];
    let out = prefix_user_message_dates(nodes, &chicago());
    // System, assistant, tool, and the named "profile" note are unchanged.
    assert_eq!(out[0].message.content, "system prompt");
    assert_eq!(out[2].message.content, "ok");
    assert_eq!(out[3].message.content, "tool result");
    assert_eq!(out[4].message.content, "switched");
    // User messages get the prefix.
    assert!(
        out[1]
            .message
            .content
            .starts_with("[2026-10-04 14:42 CDT] ")
    );
    assert!(out[1].message.content.ends_with("yesterday"));
    assert!(
        out[5]
            .message
            .content
            .starts_with("[2026-10-05 14:42 CDT] ")
    );
    assert!(out[5].message.content.ends_with("second question"));
    // The prefix is exactly `[YYYY-MM-DD HH:MM TZ] <body>` — no IANA in parens (that lives on
    // the time line of the current message, where there is room for it).
    assert!(!out[1].message.content.contains("(America/Chicago)"));
}

#[test]
fn prefix_user_message_dates_skips_compaction_tail_copies() {
    // The tail copy's `created_at` is the compaction time. Stamping it would tell the model
    // every conversation happened "just now"; the helper must skip the named tail author so the
    // summarizer's `## Timeline` section is the only date the model sees in the kept tail.
    let now = utc();
    let original_user_at = now - chrono::Duration::days(7);
    let tail_copy_at = now;
    let nodes = vec![
        node(1, Author::User, "a week ago", original_user_at),
        node(
            2,
            Author::Named("compaction-tail".into()),
            "a week ago",
            tail_copy_at,
        ),
    ];
    let out = prefix_user_message_dates(nodes, &chicago());
    // The tail copy is unprefixed.
    assert_eq!(
        out[1].message.content, "a week ago",
        "compaction tail copies must not be date-stamped"
    );
}

#[test]
fn prefix_user_message_dates_uses_the_configured_zone() {
    // Same UTC, different IANA → different prefix. Pins that the helper does not bake in
    // Chicago.
    let now = utc();
    let user_at = now - chrono::Duration::days(1);
    let nodes = vec![node(1, Author::User, "hi", user_at)];

    let chicago_prefix = prefix_user_message_dates(nodes.clone(), &chicago());
    assert!(
        chicago_prefix[0]
            .message
            .content
            .starts_with("[2026-10-04 14:42 CDT]")
    );

    let denver_prefix = prefix_user_message_dates(nodes, &denver());
    assert!(
        denver_prefix[0]
            .message
            .content
            .starts_with("[2026-10-04 13:42 MDT]")
    );
}
