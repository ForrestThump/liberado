use chrono::{TimeZone, Utc};

use super::{cron_to_english_at, ordinal};

fn at(year: i32, month: u32, day: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, 16, 0, 0).unwrap()
}

fn english(expr: &str, zone: &str, year: i32, month: u32, day: u32) -> String {
    cron_to_english_at(expr, zone, at(year, month, day))
}

fn january(expr: &str) -> String {
    english(expr, "America/Chicago", 2026, 1, 15)
}

fn july(expr: &str) -> String {
    english(expr, "America/Chicago", 2026, 7, 15)
}

#[test]
fn daily_clock_uses_the_offset_in_effect() {
    // 12:30 UTC is 6:30 AM CST in January and 7:30 AM CDT in July.
    assert_eq!(january("0 30 12 * * * *"), "Every day at 6:30 AM");
    assert_eq!(july("0 30 12 * * * *"), "Every day at 7:30 AM");
    assert_eq!(
        english("0 30 6 * * * *", "UTC", 2026, 1, 15),
        "Every day at 6:30 AM"
    );
}

#[test]
fn a_zone_without_dst_keeps_the_same_local_time() {
    let expr = "0 30 12 * * * *";
    assert_eq!(
        english(expr, "America/Phoenix", 2026, 1, 15),
        "Every day at 5:30 AM"
    );
    assert_eq!(
        english(expr, "America/Phoenix", 2026, 7, 15),
        "Every day at 5:30 AM"
    );
}

#[test]
fn weekdays_and_a_named_tuesday_use_local_wall_clock() {
    // 15:00 UTC is 9:00 AM CST. Cron 2-6 and MON-FRI are Monday through Friday.
    assert_eq!(january("0 0 15 * * 2-6 *"), "Weekdays at 9:00 AM");
    assert_eq!(january("0 0 15 * * MON-FRI *"), "Weekdays at 9:00 AM");
    assert_eq!(january("0 0 13 * * 3 *"), "Every Tuesday at 7:00 AM");
    assert_eq!(january("0 0 13 * * TUE *"), "Every Tuesday at 7:00 AM");
}

#[test]
fn a_utc_weekday_that_shifts_across_midnight_uses_the_local_day() {
    // Tuesday 02:00 UTC in July is Monday 9:00 PM CDT.
    assert_eq!(july("0 0 2 * * 3 *"), "Every Monday at 9:00 PM");
}

#[test]
fn weekdays_that_shift_off_monday_through_friday_stay_raw() {
    let expr = "0 0 2 * * 2-6 *";
    assert_eq!(july(expr), expr);
}

#[test]
fn unix_weekday_numbers_apply_only_to_five_field_expressions() {
    // 6-field: 1 is Sunday. 5-field: 1 is Monday. Both are 15:00 UTC = 9:00 AM CST.
    assert_eq!(january("0 0 15 * * 1 *"), "Every Sunday at 9:00 AM");
    assert_eq!(january("0 15 * * 1"), "Every Monday at 9:00 AM");
    assert_eq!(january("0 15 * * 0"), "Every Sunday at 9:00 AM");
    assert_eq!(january("0 15 * * 7"), "Every Sunday at 9:00 AM");
}

#[test]
fn five_field_weekdays_are_monday_through_friday() {
    assert_eq!(january("0 15 * * 1-5"), "Weekdays at 9:00 AM");
}

#[test]
fn six_field_one_through_five_is_not_called_weekdays() {
    // In this crate 1-5 is Sunday through Thursday.
    let expr = "0 0 15 * * 1-5 *";
    assert_eq!(january(expr), expr);
}

#[test]
fn the_first_of_the_month_stays_on_that_local_day() {
    assert_eq!(
        july("0 0 13 1 * * *"),
        "On the 1st of every month at 8:00 AM"
    );
    assert_eq!(
        english("0 0 0 1 * * *", "Asia/Tokyo", 2026, 7, 15),
        "On the 1st of every month at 9:00 AM"
    );
}

#[test]
fn a_month_day_that_moves_to_the_previous_local_day_stays_raw() {
    let expr = "0 0 2 1 * * *";
    assert_eq!(july(expr), expr);
    assert_eq!(july("@monthly"), "@monthly");
}

#[test]
fn ordinals_follow_english_rules() {
    assert_eq!(ordinal(1), "1st");
    assert_eq!(ordinal(2), "2nd");
    assert_eq!(ordinal(3), "3rd");
    assert_eq!(ordinal(4), "4th");
    assert_eq!(ordinal(11), "11th");
    assert_eq!(ordinal(12), "12th");
    assert_eq!(ordinal(13), "13th");
    assert_eq!(ordinal(21), "21st");
    assert_eq!(ordinal(22), "22nd");
    assert_eq!(ordinal(23), "23rd");
    assert_eq!(ordinal(31), "31st");
}

#[test]
fn intervals_do_not_need_a_timezone() {
    assert_eq!(january("0 */15 * * * *"), "Every 15 minutes");
    assert_eq!(january("0 0/15 * * * * *"), "Every 15 minutes");
    assert_eq!(january("*/15 * * * *"), "Every 15 minutes");
    assert_eq!(january("0 * * * * *"), "Every minute");
    assert_eq!(january("* * * * * *"), "Every second");
    assert_eq!(january("*/15 * * * * *"), "Every 15 seconds");
    assert_eq!(january("0 0 * * * *"), "Every hour");
    assert_eq!(january("0 0 */2 * * * *"), "Every 2 hours");
    assert_eq!(january("@hourly"), "Every hour");
    assert_eq!(
        english("0 */15 * * * *", "Not/AZone", 2026, 1, 15),
        "Every 15 minutes"
    );
}

#[test]
fn steps_that_are_not_a_constant_gap_stay_raw() {
    assert_eq!(january("*/7 * * * * *"), "*/7 * * * * *");
    assert_eq!(january("0 */7 * * * *"), "0 */7 * * * *");
    assert_eq!(january("0 0 */7 * * * *"), "0 0 */7 * * * *");
    assert_eq!(january("0 30/15 * * * *"), "0 30/15 * * * *");
}

#[test]
fn shorthands_follow_the_cron_crate_expansion() {
    assert_eq!(
        english("@daily", "UTC", 2026, 1, 15),
        "Every day at 12:00 AM"
    );
    // Sunday 00:00 UTC in July is Saturday 7:00 PM CDT.
    assert_eq!(july("@weekly"), "Every Saturday at 7:00 PM");
    assert_eq!(january("@yearly"), "@yearly");
}

#[test]
fn noon_and_midnight_use_twelve_hour_labels() {
    assert_eq!(january("0 0 6 * * * *"), "Every day at 12:00 AM");
    assert_eq!(january("0 0 18 * * * *"), "Every day at 12:00 PM");
    assert_eq!(january("0 0 1 * * * *"), "Every day at 7:00 PM");
}

#[test]
fn six_field_daily_omits_the_year() {
    assert_eq!(january("0 30 12 * * *"), "Every day at 6:30 AM");
}

#[test]
fn odd_expressions_fall_back_to_the_original_string() {
    let samples = [
        "0 0 9,17 * * * *",
        "0 0 9-17 * * * *",
        "0 0 9 * * 1,3,5 *",
        "0 0 9 * * * * 2026",
        "30 0 9 * * * *",
        "0 30 * * * *",
        "0 0 9 * * 0 *",
        "not a cron",
        "0 0 9 * * * * *",
        "@Daily",
        "  0 0 9 * JAN * *  ",
    ];
    for expr in samples {
        assert_eq!(january(expr), expr, "{expr}");
    }
}

#[test]
fn an_unknown_timezone_does_not_invent_a_clock_time() {
    let expr = "0 30 12 * * * *";
    assert_eq!(english(expr, "Not/AZone", 2026, 1, 15), expr);
    assert_eq!(english(expr, "", 2026, 1, 15), expr);
}

#[test]
fn both_day_of_month_and_day_of_week_stay_raw() {
    let expr = "0 0 15 1 * 3 *";
    assert_eq!(january(expr), expr);
}
