use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use super::{
    DoneHint, PRECISION_DATE, PRECISION_TAP, PRECISION_TICK, SOURCE_LIBERADO, SOURCE_OBSIDIAN,
    completion_stamp, completion_timing, local_noon_utc,
};

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("stamp")
        .with_timezone(&Utc)
}

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("date")
}

#[test]
fn a_hint_uses_the_tap_time_and_the_outbox_id() {
    let detected = at("2026-11-10T18:00:00Z");
    let hint = DoneHint {
        outbox_id: 7,
        resolved_at: at("2026-11-10T17:59:12Z"),
    };
    let timing = completion_timing(detected, "UTC", Some(day(2026, 11, 9)), 0, Some(&hint));
    assert_eq!(timing.precision, PRECISION_TAP);
    assert_eq!(timing.source, SOURCE_LIBERADO);
    assert_eq!(timing.outbox_id, Some(7));
    assert_eq!(timing.completed_at, hint.resolved_at);
    assert_eq!(timing.local_date, "2026-11-10");
}

#[test]
fn an_earlier_done_date_is_local_noon_when_nothing_was_closed_before() {
    let detected = at("2026-11-10T15:00:00Z");
    let timing = completion_timing(detected, "UTC", Some(day(2026, 11, 9)), 0, None);
    assert_eq!(timing.precision, PRECISION_DATE);
    assert_eq!(timing.source, SOURCE_OBSIDIAN);
    assert_eq!(timing.outbox_id, None);
    assert_eq!(timing.done_date.as_deref(), Some("2026-11-09"));
    assert_eq!(timing.local_date, "2026-11-09");
    assert_eq!(
        completion_stamp(timing.completed_at),
        "2026-11-09T12:00:00Z"
    );
}

#[test]
fn the_same_calendar_day_stays_a_tick_and_a_prior_closed_line_hides_the_old_date() {
    let detected = at("2026-11-10T15:00:00Z");
    let same_day = completion_timing(detected, "UTC", Some(day(2026, 11, 10)), 0, None);
    assert_eq!(same_day.precision, PRECISION_TICK);
    assert_eq!(same_day.completed_at, detected);
    assert_eq!(same_day.done_date.as_deref(), Some("2026-11-10"));

    let historical = completion_timing(detected, "UTC", Some(day(2026, 1, 1)), 4, None);
    assert_eq!(historical.precision, PRECISION_TICK);
    assert_eq!(historical.done_date, None);
    assert_eq!(historical.source, SOURCE_OBSIDIAN);
}

#[test]
fn chicago_noon_is_18_00_utc_after_daylight_time_ends() {
    let noon = local_noon_utc(day(2026, 11, 9), "America/Chicago");
    assert_eq!(completion_stamp(noon), "2026-11-09T18:00:00Z");
}

#[test]
fn a_done_date_is_compared_in_the_operator_zone() {
    let detected = at("2026-11-10T04:30:00Z");
    let same_local_day =
        completion_timing(detected, "America/Chicago", Some(day(2026, 11, 9)), 0, None);
    assert_eq!(same_local_day.precision, PRECISION_TICK);
    let previous_local_day =
        completion_timing(detected, "America/Chicago", Some(day(2026, 11, 8)), 0, None);
    assert_eq!(previous_local_day.precision, PRECISION_DATE);
    assert_eq!(
        completion_stamp(previous_local_day.completed_at),
        "2026-11-08T18:00:00Z"
    );
}

#[test]
fn an_unknown_zone_falls_back_to_utc() {
    let noon = local_noon_utc(day(2026, 11, 9), "Not/AZone");
    assert_eq!(noon, Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap());
}
