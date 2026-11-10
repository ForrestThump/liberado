//! Completion clock: tap time, detection time, or local noon of a done date.
//!
//! Tap time is the Done press, to the second. A tick is the minute the file
//! change was seen, plus whatever delay the sync already added. A done date
//! earlier than the detection's local date is only a date: the row stores
//! local noon so the day is right and the clock is not a guess.

use chrono::{DateTime, LocalResult, NaiveDate, TimeZone, Utc};

use crate::time::stamp;

pub const PRECISION_TAP: &str = "tap";
pub const PRECISION_TICK: &str = "tick";
pub const PRECISION_DATE: &str = "date";
pub const SOURCE_LIBERADO: &str = "liberado";
pub const SOURCE_OBSIDIAN: &str = "obsidian";

/// A Done press the tick may attribute, when `resolved_at` is recent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoneHint {
    pub outbox_id: i64,
    pub resolved_at: DateTime<Utc>,
}

/// Clock fields for one new completion row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTiming {
    pub completed_at: DateTime<Utc>,
    pub local_date: String,
    pub precision: &'static str,
    pub source: &'static str,
    pub outbox_id: Option<i64>,
    pub done_date: Option<String>,
}

/// Choose the clock for one detected completion.
///
/// `done_date` is used only when `prior_closed` is 0, so an older checked copy
/// of the same text cannot supply the date. A hint wins over that date.
pub fn completion_timing(
    detected_at: DateTime<Utc>,
    zone: &str,
    done_date: Option<NaiveDate>,
    prior_closed: i64,
    hint: Option<&DoneHint>,
) -> CompletionTiming {
    let usable = done_date.filter(|_| prior_closed == 0);
    if let Some(hint) = hint {
        return from_hint(hint, zone, usable);
    }
    if let Some(date) = earlier_done_date(usable, detected_at, zone) {
        return from_done_date(date, zone);
    }
    from_detection(detected_at, zone, usable)
}

pub fn local_date_of(at: DateTime<Utc>, zone: &str) -> String {
    at.with_timezone(&parse_zone(zone)).date_naive().to_string()
}

pub fn local_noon_utc(date: NaiveDate, zone: &str) -> DateTime<Utc> {
    let Some(naive) = date.and_hms_opt(12, 0, 0) else {
        return utc_noon(date);
    };
    match parse_zone(zone).from_local_datetime(&naive) {
        LocalResult::Single(local) => local.with_timezone(&Utc),
        LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
        LocalResult::None => utc_noon(date),
    }
}

fn from_hint(hint: &DoneHint, zone: &str, done_date: Option<NaiveDate>) -> CompletionTiming {
    CompletionTiming {
        completed_at: hint.resolved_at,
        local_date: local_date_of(hint.resolved_at, zone),
        precision: PRECISION_TAP,
        source: SOURCE_LIBERADO,
        outbox_id: Some(hint.outbox_id),
        done_date: date_text(done_date),
    }
}

fn from_done_date(date: NaiveDate, zone: &str) -> CompletionTiming {
    let completed_at = local_noon_utc(date, zone);
    CompletionTiming {
        completed_at,
        local_date: date.to_string(),
        precision: PRECISION_DATE,
        source: SOURCE_OBSIDIAN,
        outbox_id: None,
        done_date: Some(date.to_string()),
    }
}

fn from_detection(
    detected_at: DateTime<Utc>,
    zone: &str,
    done_date: Option<NaiveDate>,
) -> CompletionTiming {
    CompletionTiming {
        completed_at: detected_at,
        local_date: local_date_of(detected_at, zone),
        precision: PRECISION_TICK,
        source: SOURCE_OBSIDIAN,
        outbox_id: None,
        done_date: date_text(done_date),
    }
}

fn earlier_done_date(
    done_date: Option<NaiveDate>,
    detected_at: DateTime<Utc>,
    zone: &str,
) -> Option<NaiveDate> {
    let date = done_date?;
    let detected = detected_at.with_timezone(&parse_zone(zone)).date_naive();
    if date < detected { Some(date) } else { None }
}

fn date_text(date: Option<NaiveDate>) -> Option<String> {
    date.map(|value| value.to_string())
}

fn parse_zone(zone: &str) -> chrono_tz::Tz {
    zone.parse().unwrap_or(chrono_tz::UTC)
}

fn utc_noon(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(12, 0, 0)
        .map(|naive| naive.and_utc())
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

/// RFC3339 UTC stamp. Exposed so callers store the same text the outbox uses.
pub fn completion_stamp(at: DateTime<Utc>) -> String {
    stamp(at)
}

#[cfg(test)]
#[path = "completion_time_tests.rs"]
mod completion_time_tests;
