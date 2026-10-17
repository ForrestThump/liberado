use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;

use super::{next_fires, next_fires_after};
use crate::cron_to_english_at;

fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 0, 0, 0).unwrap()
}

fn clock(hour: u32, minute: u32) -> String {
    let (clock_hour, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{clock_hour}:{minute:02} {suffix}")
}

#[test]
fn tuesday_utc_in_summer_is_monday_evening_in_chicago() {
    let fires = next_fires_after("0 0 1 * * 3 *", "America/Chicago", 1, at(2026, 10, 16)).unwrap();
    let fire = &fires[0];
    let zone: Tz = "America/Chicago".parse().unwrap();
    let independent = fire.utc.with_timezone(&zone);
    assert_eq!(fire.local, independent);
    assert_eq!(fire.weekday, Weekday::Mon);
    assert_eq!(independent.weekday(), Weekday::Mon);
    assert_eq!(
        fire.utc,
        Utc.with_ymd_and_hms(2026, 10, 20, 1, 0, 0).unwrap()
    );
    assert_eq!(fire.display(), "Mon Oct 19, 8:00 PM CDT");
}

#[test]
fn an_empty_count_returns_no_fires() {
    let fires = next_fires_after("0 0 1 * * * *", "UTC", 0, at(2026, 1, 1)).unwrap();
    assert!(fires.is_empty());
}

#[test]
fn a_blank_zone_uses_chicago() {
    let expr = "0 0 1 * * 3 *";
    let after = at(2026, 10, 16);
    let named = next_fires_after(expr, "America/Chicago", 1, after).unwrap();
    let blank = next_fires_after(expr, "  ", 1, after).unwrap();
    assert_eq!(blank[0].local, named[0].local);
    assert_eq!(blank[0].weekday, named[0].weekday);
}

#[test]
fn a_bad_expression_or_zone_is_an_error() {
    assert!(next_fires("not a cron", "UTC", 1).is_err());
    assert!(next_fires("0 0 1 * * * *", "Not/AZone", 1).is_err());
}

#[test]
fn fires_are_later_than_the_start_and_each_other() {
    let after = at(2026, 6, 1);
    let fires = next_fires_after("0 0 * * * * *", "UTC", 4, after).unwrap();
    assert_eq!(fires.len(), 4);
    assert!(fires[0].utc > after);
    for pair in fires.windows(2) {
        assert!(pair[1].utc > pair[0].utc);
        assert_eq!(pair[0].weekday, pair[0].local.weekday());
    }
}

const ZONES: &[&str] = &[
    "America/Chicago",
    "America/New_York",
    "Europe/London",
    "Pacific/Auckland",
    "America/Phoenix",
    "UTC",
];

const SEASONS: &[(i32, u32, u32)] = &[(2026, 1, 15), (2026, 7, 15)];

#[test]
fn english_weekday_matches_the_next_fire_for_every_day_zone_and_season() {
    for dow in 1..=7 {
        let expr = format!("0 0 13 * * {dow} *");
        for zone_name in ZONES {
            let zone: Tz = zone_name.parse().unwrap();
            for (year, month, day) in SEASONS {
                let start = at(*year, *month, *day);
                let fires = next_fires_after(&expr, zone_name, 1, start).unwrap();
                let fire = &fires[0];
                let independent = fire.utc.with_timezone(&zone);
                assert_eq!(fire.local, independent, "{expr} {zone_name} {start}");
                assert_eq!(fire.weekday, independent.weekday());
                let english = cron_to_english_at(&expr, zone_name, start);
                let expected = format!(
                    "Every {} at {}",
                    fire.weekday_name(),
                    clock(independent.hour(), independent.minute())
                );
                assert_eq!(english, expected, "{expr} {zone_name} {start}");
            }
        }
    }
}

#[test]
fn chicago_fall_back_moves_the_local_hour_from_cdt_to_cst() {
    let zone: Tz = "America/Chicago".parse().unwrap();
    let fires = next_fires_after("0 30 7 * * * *", "America/Chicago", 4, at(2026, 10, 30)).unwrap();
    for fire in &fires {
        let independent = fire.utc.with_timezone(&zone);
        assert_eq!(fire.local, independent);
        assert_eq!(fire.weekday, independent.weekday());
    }
    let before = fires
        .iter()
        .find(|fire| fire.utc == Utc.with_ymd_and_hms(2026, 10, 31, 7, 30, 0).unwrap())
        .unwrap();
    let after = fires
        .iter()
        .find(|fire| fire.utc == Utc.with_ymd_and_hms(2026, 11, 1, 7, 30, 0).unwrap())
        .unwrap();
    assert_eq!(before.local.hour(), 2);
    assert_eq!(before.local.minute(), 30);
    assert_eq!(before.local.format("%Z").to_string(), "CDT");
    assert_eq!(after.local.hour(), 1);
    assert_eq!(after.local.minute(), 30);
    assert_eq!(after.local.format("%Z").to_string(), "CST");
}

#[test]
fn chicago_spring_forward_moves_the_local_hour_from_cst_to_cdt() {
    let zone: Tz = "America/Chicago".parse().unwrap();
    let fires = next_fires_after("0 30 7 * * * *", "America/Chicago", 5, at(2026, 3, 6)).unwrap();
    for fire in &fires {
        let independent = fire.utc.with_timezone(&zone);
        assert_eq!(fire.local, independent);
        assert_eq!(fire.weekday, independent.weekday());
    }
    let before = fires
        .iter()
        .find(|fire| fire.utc == Utc.with_ymd_and_hms(2026, 3, 7, 7, 30, 0).unwrap())
        .unwrap();
    let after = fires
        .iter()
        .find(|fire| fire.utc == Utc.with_ymd_and_hms(2026, 3, 9, 7, 30, 0).unwrap())
        .unwrap();
    assert_eq!(before.local.hour(), 1);
    assert_eq!(before.local.format("%Z").to_string(), "CST");
    assert_eq!(after.local.hour(), 2);
    assert_eq!(after.local.format("%Z").to_string(), "CDT");
}
