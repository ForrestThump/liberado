use chrono::{DateTime, Datelike, Offset, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use liberado_common::{
    CivilSchedule, civil_english, compile_cron, parse_civil, parse_compiled_cron,
};

use super::next_fires_zoned_after;
use crate::cron_to_english_civil;

fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
}

fn owned(days: &[&str]) -> Vec<String> {
    days.iter().map(|day| (*day).to_string()).collect()
}

/// Independent of the cron crate: walk local midnights and keep unique civil times.
fn reference_until(
    spec: &CivilSchedule,
    zone: Tz,
    after: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let mut out = Vec::new();
    let mut date = after.with_timezone(&zone).date_naive();
    let last = horizon.with_timezone(&zone).date_naive();
    while date <= last {
        if spec.days.contains(&date.weekday())
            && let Some(naive) = date.and_hms_opt(spec.hour, spec.minute, spec.second)
            && let Some(local) = zone.from_local_datetime(&naive).single()
        {
            let utc = local.with_timezone(&Utc);
            if utc > after && utc < horizon {
                out.push(utc);
            }
        }
        date = date.succ_opt().expect("date");
    }
    out
}

fn fires_until(
    expr: &str,
    zone_name: &str,
    after: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    next_fires_zoned_after(expr, zone_name, 800, after)
        .unwrap()
        .into_iter()
        .map(|fire| fire.utc)
        .filter(|utc| *utc < horizon)
        .collect()
}

fn assert_same_instants(got: &[DateTime<Utc>], expect: &[DateTime<Utc>], label: &str) {
    assert_eq!(got.len(), expect.len(), "{label}");
    for (index, (left, right)) in got.iter().zip(expect).enumerate() {
        assert_eq!(left, right, "{label} at {index}");
    }
}

fn month_present(fires: &[DateTime<Utc>], zone: Tz, year: i32, month: u32) -> bool {
    fires.iter().any(|utc| {
        let local = utc.with_timezone(&zone);
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
        let zone: Tz = zone_name.parse().unwrap();
        for days in day_sets {
            let spec = parse_civil(&owned(days), "20:00").unwrap();
            let expr = compile_cron(&spec);
            assert_eq!(parse_compiled_cron(&expr), Some(spec.clone()));
            let got = fires_until(&expr, zone_name, start, horizon);
            let expect = reference_until(&spec, zone, start, horizon);
            assert_same_instants(&got, &expect, &format!("{zone_name} {days:?}"));
            assert!(got.len() > 50, "{zone_name} {days:?} {}", got.len());
        }
    }
    let chicago: Tz = "America/Chicago".parse().unwrap();
    let tue = fires_until(
        &compile_cron(&parse_civil(&owned(&["tue"]), "20:00").unwrap()),
        "America/Chicago",
        start,
        horizon,
    );
    for (year, month) in [(2026, 3), (2026, 11), (2027, 3), (2027, 11)] {
        assert!(
            month_present(&tue, chicago, year, month),
            "missing {year}-{month}"
        );
    }
    let sydney: Tz = "Australia/Sydney".parse().unwrap();
    let sydney_tue = fires_until(
        &compile_cron(&parse_civil(&owned(&["tue"]), "20:00").unwrap()),
        "Australia/Sydney",
        start,
        horizon,
    );
    for (year, month) in [(2026, 4), (2026, 10), (2027, 4), (2027, 10)] {
        assert!(
            month_present(&sydney_tue, sydney, year, month),
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
    assert_eq!(before.weekday, Weekday::Tue);
    assert_eq!(after.weekday, Weekday::Tue);
    assert_eq!(before.local.offset().fix().local_minus_utc(), -5 * 3600);
    assert_eq!(after.local.offset().fix().local_minus_utc(), -6 * 3600);
}

#[test]
fn a_cron_expression_with_a_zone_uses_civil_fields() {
    let zoned =
        next_fires_zoned_after("0 0 20 * * 3 *", "America/Chicago", 1, at(2026, 10, 1)).unwrap();
    let utc =
        crate::next_fires_after("0 0 20 * * 3 *", "America/Chicago", 1, at(2026, 10, 1)).unwrap();
    assert_eq!(zoned[0].local.hour(), 20);
    assert_eq!(zoned[0].weekday, Weekday::Tue);
    assert_ne!(utc[0].local.hour(), zoned[0].local.hour());
}

fn at_hms(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, minute, second)
        .unwrap()
}
