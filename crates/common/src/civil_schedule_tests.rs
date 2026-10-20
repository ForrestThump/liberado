use chrono::Weekday;

use super::{CivilSchedule, civil_english, compile_cron, parse_civil, parse_compiled_cron};

fn spec(days: &[&str], at: &str) -> CivilSchedule {
    let owned: Vec<String> = days.iter().map(|day| (*day).to_string()).collect();
    parse_civil(&owned, at).unwrap()
}

#[test]
fn tuesday_at_eight_compiles_and_reads_back() {
    let parsed = spec(&["tue"], "20:00");
    assert_eq!(parsed.days, vec![Weekday::Tue]);
    assert_eq!((parsed.hour, parsed.minute, parsed.second), (20, 0, 0));
    let expr = compile_cron(&parsed);
    assert_eq!(expr, "0 0 20 * * 3 *");
    assert_eq!(parse_compiled_cron(&expr), Some(parsed.clone()));
    assert_eq!(
        civil_english(&parsed, "America/Chicago"),
        "Every Tuesday at 8:00 PM America/Chicago"
    );
}

#[test]
fn day_aliases_and_order_round_trip_to_the_same_spec() {
    let left = spec(&["Friday", "mon", "Tues"], "08:05:09");
    let right = spec(&["monday", "tues", "fri"], "8:05:09");
    assert_eq!(left, right);
    assert_eq!(left.days, vec![Weekday::Mon, Weekday::Tue, Weekday::Fri]);
    assert_eq!(parse_compiled_cron(&compile_cron(&left)), Some(left));
}

#[test]
fn a_full_week_and_weekdays_use_short_phrases() {
    let week = spec(&["sun", "mon", "tue", "wed", "thu", "fri", "sat"], "20:00");
    assert_eq!(civil_english(&week, "UTC"), "Every day at 8:00 PM UTC");
    assert_eq!(parse_compiled_cron(&compile_cron(&week)), Some(week));
    let days = spec(&["fri", "mon", "thu", "wed", "tue"], "07:00");
    assert_eq!(
        civil_english(&days, "Europe/London"),
        "Weekdays at 7:00 AM Europe/London"
    );
    let star = parse_compiled_cron("0 0 20 * * * *").unwrap();
    assert_eq!(star.days.len(), 7);
    assert_eq!(
        civil_english(&star, "America/Chicago"),
        "Every day at 8:00 PM America/Chicago"
    );
}

#[test]
fn days_and_at_reject_bad_tokens() {
    let cases = [
        (vec!["funday".to_string()], "20:00"),
        (vec!["tue".to_string(), "tue".to_string()], "20:00"),
        (vec![], "20:00"),
        (vec!["tue".to_string()], "25:00"),
        (vec!["tue".to_string()], "20"),
        (vec!["tue".to_string()], "8:00 PM"),
        (vec!["tue".to_string()], ""),
        (vec!["tue".to_string()], "20:60"),
        (vec!["  ".to_string()], "20:00"),
    ];
    for (days, at) in cases {
        assert!(parse_civil(&days, at).is_err(), "{days:?} {at}");
    }
}
