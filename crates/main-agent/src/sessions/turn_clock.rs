//! Wall-clock context for interactive chat turns.
//!
//! Before this module, the model had no reliable way to know what time it was for the human on the
//! other end: a turn that started on Monday and resumed Thursday would see the *old* tool result's
//! relative dates and guess "today" from those, writing "2026-10-03" for something the user said was
//! due "tomorrow at noon" on 2026-10-05. Cron and webhook firings already stamped
//! [`UserTimezone`] on their goal text; the chat path never did.
//!
//! The fix is deliberately **ephemeral**: the framed text reaches the model and nothing else.
//! `persist_user_message` still writes the raw text, `history()` / `history_nodes()` still return
//! the raw text, and rendered WebUI history is unchanged. Only the in-memory view the model reads
//! carries the local-time and gap framing.
//!
//! Why the *current user message* is the only thing framed, instead of a transient system message
//! at the front:
//!
//! 1. **Provider prompt cache.** Every leading system block invalidates the cache for the whole
//!    history on every turn; framing the current user message touches only the tail, so a multi-hour
//!    chat with one tool call stays on a stable cache key.
//! 2. **Transient accounting.** `Conversation::turn_tail(before)` skips exactly
//!    `transient` leading system messages; inserting a framed system message would shift that
//!    offset, and the persisted tail would start inside the *old* history.
//! 3. **Recency.** The most recent instruction the model sees, after every older turn and the tool
//!    manifest, is the right place to put "the clock now is X, the last question was Y ago".
//!
//! See [`ChatSessions::with_user_timezone`](super::ChatSessions::with_user_timezone) for the wiring
//! and the call-site that frames the current user message in [`turn`](super::ChatSessions::turn)
//! and [`turn_stream`](super::ChatSessions::turn_stream).

use chrono::{DateTime, Duration, Utc};
use liberado_common::UserTimezone;
use liberado_conversation_store::{Author, MessageNode};
use liberado_provider::Message;

use super::ChatSessions;

/// The default gap below which the model is not told "the last message was N ago". A user who
/// reopens a chat after lunch should not see "Last user message was 6 hours ago"; the conversation
/// itself is fresh enough that no framing is needed.
///
/// 12 hours is the deliberate line: it covers a full afternoon/evening gap (when real-world
/// assumptions do not drift) without leaking into multi-day spans where they do.
pub(super) const DEFAULT_GAP_THRESHOLD_HOURS: i64 = 12;

/// How long the model remembers as "still close in time" — see [`DEFAULT_GAP_THRESHOLD_HOURS`].
pub(super) fn default_gap_threshold() -> Duration {
    Duration::hours(DEFAULT_GAP_THRESHOLD_HOURS)
}

/// The most recent `Author::User` node's `created_at`, or `None` when there is no prior user
/// message in the loaded history.
///
/// Only `Author::User` counts. Every other author is ignored: a compaction tail copy
/// ([`Author::Named`] with name `compaction-tail`) was re-appended with `created_at = now` and
/// would lie about the gap; a profile switch note, a subagent handoff, or any other
/// [`Author::Named`] is not user-typed text; and `Author::System` / `Author::Assistant` /
/// `Author::Tool` were not written by the human.
pub(super) fn previous_user_created_at(nodes: &[MessageNode]) -> Option<DateTime<Utc>> {
    nodes
        .iter()
        .rev()
        .find(|n| matches!(n.author, Author::User))
        .map(|n| n.created_at)
}

/// Format a UTC instant as the same `[YYYY-MM-DD HH:MM TZ]` prefix the model will see stamped on
/// prior user messages. Kept in sync with [`UserTimezone::context_line_at`]'s body so the prefix
/// reads as "the same clock" wherever it appears in context.
pub(super) fn format_user_date_prefix(tz: &UserTimezone, utc: DateTime<Utc>) -> String {
    let local = tz.at(utc);
    format!(
        "[{} {}] ",
        local.format("%Y-%m-%d %H:%M"),
        local.format("%Z"),
    )
}

/// Render a `now - previous` duration as the humanized string for the gap line.
///
/// Picks the highest non-zero unit, then includes the next one if present:
///
/// * `< 1 minute`     → "less than a minute"
/// * `< 1 hour`       → "5 minutes" / "1 minute"
/// * `< 1 day`        → "5 hours" / "1 hour", optionally "+ 30 minutes" when there is a non-zero
///   minute remainder and the hour is non-zero (so a 1h 5m gap does not round to "1 hour" and
///   lose the minutes that proved the gap is more than an hour). Day-level gaps are precise to
///   the hour; a partial day at that scale is noise.
/// * `>= 1 day`       → "2 days 6 hours" / "1 day 1 hour" / "1 day" / "5 days".
///
/// Pure: no I/O, no allocation outside the returned `String`. The exact thresholds and unit names
/// are part of the contract — the gap line is a string the model has to read, and reordering the
/// units or rounding the day boundary would change what the model believes about the gap.
pub(super) fn humanize_gap(delta: Duration) -> String {
    let secs = delta.num_seconds().max(0);
    if secs < 60 {
        return "less than a minute".to_string();
    }
    let total_minutes = secs / 60;
    if total_minutes < 60 {
        return plural_unit(total_minutes, "minute");
    }
    let total_hours = total_minutes / 60;
    let minute_remainder = total_minutes % 60;
    if total_hours < 24 {
        let hours_str = plural_unit(total_hours, "hour");
        if minute_remainder == 0 {
            return hours_str;
        }
        return format!("{hours_str} {}", plural_unit(minute_remainder, "minute"));
    }
    let days = total_hours / 24;
    let hour_remainder = total_hours % 24;
    let days_str = plural_unit(days, "day");
    if hour_remainder == 0 {
        days_str
    } else {
        format!("{days_str} {}", plural_unit(hour_remainder, "hour"))
    }
}

fn plural_unit(n: i64, unit: &str) -> String {
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// The gap line — `Last user message was 2 days 6 hours ago (2026-10-03 13:36 CDT).` — or `None`
/// when no previous user message exists, or when the gap is at or below `threshold`.
///
/// A gap of exactly `threshold` is **not** enough to trigger the line: the line is a "this is
/// stale enough to be worth telling the model about" hint, and a gap that has just crossed the
/// line is the borderline case the model can read off the time line alone. Strict `>` keeps the
/// threshold from being a coin-flip on the wire.
///
/// `previous` must be the timestamp of the *original* user node (see
/// [`previous_user_created_at`]); a compaction-tail copy's `created_at` is the compaction time,
/// not the typing time, and would lie about the gap forever.
///
/// `now` is an injected instant so tests can pin the output without sleeping.
pub(super) fn compute_gap_line(
    tz: &UserTimezone,
    now: DateTime<Utc>,
    previous: Option<DateTime<Utc>>,
    threshold: Duration,
) -> Option<String> {
    let prev = previous?;
    let delta = now.signed_duration_since(prev);
    if delta <= threshold {
        return None;
    }
    let humanized = humanize_gap(delta);
    let prev_local = format_user_date_prefix(tz, prev).trim_end().to_string();
    Some(format!(
        "Last user message was {humanized} ago ({prev_local})."
    ))
}

/// Build the framed text the model sees: `Local time: ...\n\n[optional gap line]\n\n<raw user>`.
///
/// `None` `tz` → returns the raw `user` text unchanged, the "no timezone attached" path the
/// existing test surface expects. The behavior is then byte-identical to today, which is the
/// property the pre-existing tests assert on and the reason this whole feature is a no-op when
/// no `UserTimezone` has been wired.
pub(super) fn frame_user_message(
    tz: Option<&UserTimezone>,
    now: DateTime<Utc>,
    previous_user: Option<DateTime<Utc>>,
    threshold: Duration,
    raw_user: &str,
) -> String {
    let Some(tz) = tz else {
        return raw_user.to_string();
    };
    let time_line = tz.context_line_at(now);
    let gap_line = compute_gap_line(tz, now, previous_user, threshold);
    let mut parts: Vec<&str> = vec![time_line.as_str()];
    if let Some(gap) = gap_line.as_deref() {
        parts.push(gap);
    }
    parts.push(raw_user);
    parts.join("\n\n")
}

/// Date-prefix every `Author::User` message in `nodes`. Returns a new `Vec`; the input is not
/// mutated.
///
/// Compaction-tail copies are **not** prefixed: their `created_at` is the compaction time, and
/// stamping that on the (otherwise verbatim) tail copy would tell the model the conversation
/// happened in the last few seconds — which is what makes a long chat read as "all just now".
/// The model-visible view in `maybe_compact` skips the originals entirely (they are in the
/// elided region), so the tail copy's date stamp would be the only one in the visible history;
/// keeping it un-prefixed leaves the model to look at the date the summarizer carried in the
/// `## Timeline` section instead.
///
/// `Author::System`, `Author::Assistant`, `Author::Tool`, and any other `Author::Named` are left
/// alone — only the human's own text is dated.
pub(super) fn prefix_user_message_dates(
    nodes: Vec<MessageNode>,
    tz: &UserTimezone,
) -> Vec<MessageNode> {
    nodes
        .into_iter()
        .map(|mut node| {
            let is_user = matches!(node.author, Author::User);
            if !is_user {
                return node;
            }
            let prefix = format_user_date_prefix(tz, node.created_at);
            // The prefix is the same shape on every line of one node: content is single-message
            // text. Prepending once is enough.
            node.message.content = format!("{prefix}{}", node.message.content);
            node
        })
        .collect()
}

#[cfg(test)]
#[path = "turn_clock_tests.rs"]
mod tests;

impl ChatSessions {
    /// Attach the operator's IANA timezone so every interactive turn is framed with
    /// `Local time: ...` (and, when a previous user message is far enough back, the gap line).
    ///
    /// `None` (the default) keeps the raw text path every existing test asserts on; the chat
    /// surface, like every other untimed turn, gets the model with no wall-clock context. An
    /// invalid IANA name never reaches here — `Config::validate` rejects unknown zones at load
    /// and the server only calls this with the resolved `UserTimezone` from
    /// `topology.user_timezone()`.
    pub fn with_user_timezone(mut self, tz: UserTimezone) -> Self {
        self.user_timezone = Some(tz);
        self
    }

    /// The currently-attached operator timezone, or `None` when this `ChatSessions` was built
    /// without one. Tests assert on this to prove framing is / is not in effect.
    #[cfg(test)]
    pub(crate) fn user_timezone(&self) -> Option<UserTimezone> {
        self.user_timezone
    }

    /// Build the model-visible copy of the current user message: `Local time: ...` (and, when
    /// `previous_user` is far enough back, the gap line) plus the raw text. When no timezone is
    /// attached, the raw text passes through unchanged.
    ///
    /// `previous_user` is the `created_at` of the most recent `Author::User` node from the
    /// *pre-compaction* load — [`previous_user_created_at`] returns the right value, and the
    /// framing must happen before `maybe_compact` consumes the loaded nodes.
    pub(super) fn frame_user_message(
        &self,
        now: DateTime<Utc>,
        previous_user: Option<DateTime<Utc>>,
        raw_user: &str,
    ) -> String {
        frame_user_message(
            self.user_timezone.as_ref(),
            now,
            previous_user,
            default_gap_threshold(),
            raw_user,
        )
    }

    /// Map loaded `nodes` to the model-visible `Vec<Message>` the model will read for a turn.
    /// When a `UserTimezone` is attached, prior `Author::User` messages are date-prefixed (the
    /// summarizer's elided slice carries the same prefixes, so rolling summaries can keep the
    /// timeline). The persisted nodes and `history()` / `history_nodes()` are unchanged.
    pub(super) fn model_view_messages(&self, nodes: &[MessageNode]) -> Vec<Message> {
        match self.user_timezone.as_ref() {
            Some(tz) => prefix_user_message_dates(nodes.to_vec(), tz)
                .into_iter()
                .map(|n| n.message)
                .collect(),
            None => nodes.iter().map(|n| n.message.clone()).collect(),
        }
    }
}
