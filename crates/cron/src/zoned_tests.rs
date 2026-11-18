use jiff::civil::{DateTime, Weekday, date};
use jiff::tz::TimeZone;
use liberado_common::{
    CivilSchedule, UtcStamp, civil_english, compile_cron, parse_civil, parse_compiled_cron,
};

use super::next_fires_zoned_after;
use crate::cron_to_english_civil;

fn at(year: i16, month: i8, day: i8) -> UtcStamp {
    UtcStamp::from_civil_utc(year, month, day, 0, 0, 0).unwrap()
}

fn owned(days: &[&str]) -> Vec<String> {
    days.iter().map(|day| (*day).to_string()).collect()
}

fn zone(name: &str) -> TimeZone {
    TimeZone::get(name).unwrap()
}

/// Independent of jiff-cron: walk local midnights and keep unique civil times.
///
/// `unambiguous` is the old `.single()` check. A gap or a fold is not a fire
/// for this reference. 20:00 never lands in one, so the Tuesday walk still
/// matches the schedule.
fn reference_until(
    spec: &CivilSchedule,
    zone: &TimeZone,
    after: UtcStamp,
    horizon: UtcStamp,
) -> Vec<UtcStamp> {
    let mut out = Vec::new();
    let mut date = after.timestamp().to_zoned(zone.clone()).date();
    let last = horizon.timestamp().to_zoned(zone.clone()).date();
    while date <= last {
        if spec.days.contains(&date.weekday())
            && let Some(civil) = clock_on(date, spec)
            && let Ok(local) = zone.to_ambiguous_zoned(civil).unambiguous()
        {
            let utc = UtcStamp::from_timestamp(local.timestamp());
            if utc > after && utc < horizon {
                out.push(utc);
            }
        }
        date = date.tomorrow().expect("date");
    }
    out
}

fn clock_on(date: jiff::civil::Date, spec: &CivilSchedule) -> Option<DateTime> {
    let hour = i8::try_from(spec.hour).ok()?;
    let minute = i8::try_from(spec.minute).ok()?;
    let second = i8::try_from(spec.second).ok()?;
    DateTime::new(
        date.year(),
        date.month(),
        date.day(),
        hour,
        minute,
        second,
        0,
    )
    .ok()
}

fn fires_until(expr: &str, zone_name: &str, after: UtcStamp, horizon: UtcStamp) -> Vec<UtcStamp> {
    next_fires_zoned_after(expr, zone_name, 800, after)
        .unwrap()
        .into_iter()
        .map(|fire| fire.utc)
        .filter(|utc| *utc < horizon)
        .collect()
}

fn assert_same_instants(got: &[UtcStamp], expect: &[UtcStamp], label: &str) {
    assert_eq!(got.len(), expect.len(), "{label}");
    for (index, (left, right)) in got.iter().zip(expect).enumerate() {
        assert_eq!(left, right, "{label} at {index}");
    }
}

fn month_present(fires: &[UtcStamp], zone: &TimeZone, year: i16, month: i8) -> bool {
    fires.iter().any(|utc| {
        let local = utc.timestamp().to_zoned(zone.clone());
        local.year() == year && local.month() == month
    })
}

#[test]
fn compiled_civil_fires_match_an_independent_zone_walk() {
    let zones = [
        "America/Chicago",
        "America/New_York",
        "Europe/London",
        "Australia/Sydney",
        "Pacific/Auckland",
    ];
    let day_sets: &[&[&str]] = &[
        &["tue"],
        &["mon", "wed", "fri"],
        &["sun"],
        &["mon", "tue", "wed", "thu", "fri", "sat", "sun"],
    ];
    let start = at(2026, 1, 1);
    let horizon = at(2028, 1, 1);
    for zone_name in zones {
        let zone = zone(zone_name);
        for days in day_sets {
            let spec = parse_civil(&owned(days), "20:00").unwrap();
            let expr = compile_cron(&spec);
            assert_eq!(parse_compiled_cron(&expr), Some(spec.clone()));
            let got = fires_until(&expr, zone_name, start, horizon);
            let expect = reference_until(&spec, &zone, start, horizon);
            assert_same_instants(&got, &expect, &format!("{zone_name} {days:?}"));
            assert!(got.len() > 50, "{zone_name} {days:?} {}", got.len());
        }
    }
    let chicago = zone("America/Chicago");
    let tue = fires_until(
        &compile_cron(&parse_civil(&owned(&["tue"]), "20:00").unwrap()),
        "America/Chicago",
        start,
        horizon,
    );
    for (year, month) in [(2026, 3), (2026, 11), (2027, 3), (2027, 11)] {
        assert!(
            month_present(&tue, &chicago, year, month),
            "missing {year}-{month}"
        );
    }
    let sydney = zone("Australia/Sydney");
    let sydney_tue = fires_until(
        &compile_cron(&parse_civil(&owned(&["tue"]), "20:00").unwrap()),
        "Australia/Sydney",
        start,
        horizon,
    );
    for (year, month) in [(2026, 4), (2026, 10), (2027, 4), (2027, 10)] {
        assert!(
            month_present(&sydney_tue, &sydney, year, month),
            "missing Sydney {year}-{month}"
        );
    }
}

#[test]
fn chicago_tuesday_8pm_stays_8pm_across_november_2026() {
    let spec = parse_civil(&owned(&["tue"]), "20:00").unwrap();
    let expr = compile_cron(&spec);
    assert_eq!(
        civil_english(&spec, "America/Chicago"),
        "Every Tuesday at 8:00 PM America/Chicago"
    );
    assert_eq!(
        cron_to_english_civil(&expr, "America/Chicago"),
        "Every Tuesday at 8:00 PM America/Chicago"
    );
    let fires = next_fires_zoned_after(&expr, "America/Chicago", 8, at(2026, 10, 1)).unwrap();
    let before = fires
        .iter()
        .find(|fire| fire.utc == at_hms(2026, 10, 28, 1, 0, 0))
        .expect("CDT tuesday");
    let after = fires
        .iter()
        .find(|fire| fire.utc == at_hms(2026, 11, 4, 2, 0, 0))
        .expect("CST tuesday");
    assert_eq!(before.local.hour(), 20);
    assert_eq!(after.local.hour(), 20);
    assert_eq!(before.local.minute(), 0);
    assert_eq!(after.local.minute(), 0);
    assert_eq!(before.weekday, Weekday::Tuesday);
    assert_eq!(after.weekday, Weekday::Tuesday);
    assert_eq!(before.local.offset().seconds(), -5 * 3600);
    assert_eq!(after.local.offset().seconds(), -6 * 3600);
}

#[test]
fn a_cron_expression_with_a_zone_uses_civil_fields() {
    let zoned =
        next_fires_zoned_after("0 0 20 * * 3 *", "America/Chicago", 1, at(2026, 10, 1)).unwrap();
    let utc =
        crate::next_fires_after("0 0 20 * * 3 *", "America/Chicago", 1, at(2026, 10, 1)).unwrap();
    assert_eq!(zoned[0].local.hour(), 20);
    assert_eq!(zoned[0].weekday, Weekday::Tuesday);
    assert_ne!(utc[0].local.hour(), zoned[0].local.hour());
}

/// Compatible disambiguation, not the old cron crate's skip-the-day behavior.
/// A civil time in the spring gap is shifted one hour later.
#[test]
fn chicago_spring_gap_shifts_0230_one_hour_later() {
    let fires =
        next_fires_zoned_after("0 30 2 * * * *", "America/Chicago", 8, at(2026, 3, 6)).unwrap();
    let on_gap_day: Vec<_> = fires
        .iter()
        .filter(|fire| fire.local.date() == date(2026, 3, 8))
        .collect();
    assert_eq!(on_gap_day.len(), 1);
    assert_eq!(
        on_gap_day[0].local.to_string(),
        "2026-03-08T03:30:00-05:00[America/Chicago]"
    );
}

/// Compatible disambiguation, not the old cron crate's skip-the-day behavior.
/// The fall overlap fires once, at the CST occurrence.
#[test]
fn chicago_fall_overlap_fires_0130_once_at_cst() {
    let fires =
        next_fires_zoned_after("0 30 1 * * * *", "America/Chicago", 8, at(2026, 10, 30)).unwrap();
    let on_overlap_day: Vec<_> = fires
        .iter()
        .filter(|fire| fire.local.date() == date(2026, 11, 1))
        .collect();
    assert_eq!(on_overlap_day.len(), 1);
    assert_eq!(
        on_overlap_day[0].local.to_string(),
        "2026-11-01T01:30:00-06:00[America/Chicago]"
    );
}

fn at_hms(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> UtcStamp {
    UtcStamp::from_civil_utc(year, month, day, hour, minute, second).unwrap()
}
