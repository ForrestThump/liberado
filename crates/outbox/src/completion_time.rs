//! Completion clock: tap time, detection time, or local noon of a done date.
//!
//! Tap time is the Done press, to the second. A tick is the minute the file
//! change was seen, plus whatever delay the sync already added. A done date
//! earlier than the detection's local date is only a date: the row stores
//! local noon so the day is right and the clock is not a guess.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use liberado_common::UtcStamp;

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
    pub resolved_at: UtcStamp,
}

/// Clock fields for one new completion row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTiming {
    pub completed_at: UtcStamp,
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
    detected_at: UtcStamp,
    zone: &str,
    done_date: Option<Date>,
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

pub fn local_date_of(at: UtcStamp, zone: &str) -> String {
    at.timestamp().to_zoned(parse_zone(zone)).date().to_string()
}

pub fn local_noon_utc(date: Date, zone: &str) -> UtcStamp {
    let civil = date.to_datetime(jiff::civil::time(12, 0, 0, 0));
    match civil.to_zoned(parse_zone(zone)) {
        Ok(local) => UtcStamp::from_timestamp(local.timestamp()),
        Err(_) => utc_noon(date),
    }
}

fn from_hint(hint: &DoneHint, zone: &str, done_date: Option<Date>) -> CompletionTiming {
    CompletionTiming {
        completed_at: hint.resolved_at,
        local_date: local_date_of(hint.resolved_at, zone),
        precision: PRECISION_TAP,
        source: SOURCE_LIBERADO,
        outbox_id: Some(hint.outbox_id),
        done_date: date_text(done_date),
    }
}

fn from_done_date(date: Date, zone: &str) -> CompletionTiming {
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

fn from_detection(detected_at: UtcStamp, zone: &str, done_date: Option<Date>) -> CompletionTiming {
    CompletionTiming {
        completed_at: detected_at,
        local_date: local_date_of(detected_at, zone),
        precision: PRECISION_TICK,
        source: SOURCE_OBSIDIAN,
        outbox_id: None,
        done_date: date_text(done_date),
    }
}

fn earlier_done_date(done_date: Option<Date>, detected_at: UtcStamp, zone: &str) -> Option<Date> {
    let date = done_date?;
    let detected = detected_at.timestamp().to_zoned(parse_zone(zone)).date();
    if date < detected { Some(date) } else { None }
}

fn date_text(date: Option<Date>) -> Option<String> {
    date.map(|value| value.to_string())
}

fn parse_zone(zone: &str) -> TimeZone {
    TimeZone::get(zone).unwrap_or(TimeZone::UTC)
}

fn utc_noon(date: Date) -> UtcStamp {
    date.to_datetime(jiff::civil::time(12, 0, 0, 0))
        .to_zoned(TimeZone::UTC)
        .ok()
        .map(|zoned| UtcStamp::from_timestamp(zoned.timestamp()))
        .unwrap_or_default()
}

/// RFC3339 UTC stamp. Exposed so callers store the same text the outbox uses.
pub fn completion_stamp(at: UtcStamp) -> String {
    stamp(at)
}

#[cfg(test)]
#[path = "completion_time_tests.rs"]
mod completion_time_tests;
