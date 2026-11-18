//! Next-occurrence date math for recurring tasks, following the Obsidian Tasks
//! plugin convention. Ported from the historical task tooling; lives in the
//! module (not core) for the same reason as the renderer — recurrence is a
//! Tasks-plugin convention and its only consumer is `tasks_complete`.

use jiff::ToSpan;
use jiff::civil::{Date, Weekday};

/// Compute `(due, scheduled, start)` for the next instance of a recurring task,
/// offset by the same interval as the due date. Returns `None` when the rule is
/// not recognized.
///
/// Supported units: `day(s)`, `week(s)`, `month(s)`, `year(s)`, `weekday(s)`,
/// `weekend(s)`, with an optional leading count (e.g. `every 2 weeks`). A leading
/// `!` ("when done") is accepted and ignored, matching the reference behavior.
pub fn compute_next_occurrence(
    recurrence: &str,
    due_date: Option<Date>,
    scheduled_date: Option<Date>,
    start_date: Option<Date>,
    done_date: Date,
) -> Option<(Option<Date>, Option<Date>, Option<Date>)> {
    let lower = recurrence.trim().to_lowercase();
    let rest = lower.strip_prefix("every ").unwrap_or(&lower).trim();
    let rest = rest.trim_start_matches('!').trim();

    let (count, unit): (i64, &str) = match rest.find(' ') {
        Some(pos) => match rest[..pos].parse::<i64>() {
            Ok(count) => (count, rest[pos + 1..].trim()),
            Err(_) => (1, rest),
        },
        None => (1, rest),
    };

    // The due date anchors the interval; fall back to the completion date.
    let reference = due_date.unwrap_or(done_date);

    let next_due: Date = match unit {
        "day" | "days" => reference.checked_add(count.days()).ok()?,
        "week" | "weeks" => reference.checked_add((count * 7).days()).ok()?,
        "month" | "months" => advance_by_months(reference, count)?,
        "year" | "years" => advance_by_months(reference, count.checked_mul(12)?)?,
        "weekday" | "weekdays" => next_weekday(reference)?,
        "weekend" | "weekends" => next_weekend(reference)?,
        _ => return None,
    };

    let offset = reference.until(next_due).ok()?;
    let next_scheduled = scheduled_date.and_then(|date| date.checked_add(offset).ok());
    let next_start = start_date.and_then(|date| date.checked_add(offset).ok());
    Some((Some(next_due), next_scheduled, next_start))
}

/// Advance a date by `months` calendar months, clamping the day to the last
/// valid day of the target month (so Jan 31 + 1 month → Feb 28/29).
fn advance_by_months(date: Date, months: i64) -> Option<Date> {
    let total = i64::from(date.month()) - 1 + months;
    let new_year = i64::from(date.year()) + total.div_euclid(12);
    let new_month = total.rem_euclid(12) + 1;
    let year = i16::try_from(new_year).ok()?;
    let month = i8::try_from(new_month).ok()?;
    let first = Date::new(year, month, 1).ok()?;
    let last = first.last_of_month().day();
    let day = date.day().min(last);
    Date::new(year, month, day).ok()
}

fn next_weekday(reference: Date) -> Option<Date> {
    let mut day = reference.checked_add(1.days()).ok()?;
    while matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday) {
        day = day.checked_add(1.days()).ok()?;
    }
    Some(day)
}

fn next_weekend(reference: Date) -> Option<Date> {
    let mut day = reference.checked_add(1.days()).ok()?;
    while !matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday) {
        day = day.checked_add(1.days()).ok()?;
    }
    Some(day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> Date {
        text.parse().unwrap()
    }

    #[test]
    fn every_day_advances_the_due_date() {
        let (due, _, _) = compute_next_occurrence(
            "every day",
            Some(date("2026-07-19")),
            None,
            None,
            date("2026-07-19"),
        )
        .unwrap();
        assert_eq!(due, Some(date("2026-07-20")));
    }

    #[test]
    fn honors_a_leading_count() {
        let (due, _, _) = compute_next_occurrence(
            "every 2 weeks",
            Some(date("2026-07-01")),
            None,
            None,
            date("2026-07-01"),
        )
        .unwrap();
        assert_eq!(due, Some(date("2026-07-15")));
    }

    #[test]
    fn month_clamps_to_last_valid_day() {
        let (due, _, _) = compute_next_occurrence(
            "every month",
            Some(date("2026-01-31")),
            None,
            None,
            date("2026-01-31"),
        )
        .unwrap();
        assert_eq!(due, Some(date("2026-02-28")));
    }

    #[test]
    fn scheduled_and_start_shift_by_the_same_offset() {
        let (due, scheduled, start) = compute_next_occurrence(
            "every week",
            Some(date("2026-07-10")),
            Some(date("2026-07-08")),
            Some(date("2026-07-07")),
            date("2026-07-10"),
        )
        .unwrap();
        assert_eq!(due, Some(date("2026-07-17")));
        assert_eq!(scheduled, Some(date("2026-07-15")));
        assert_eq!(start, Some(date("2026-07-14")));
    }

    #[test]
    fn weekday_skips_the_weekend() {
        // 2026-07-17 is a Friday; the next weekday is Monday the 20th.
        let (due, _, _) = compute_next_occurrence(
            "every weekday",
            Some(date("2026-07-17")),
            None,
            None,
            date("2026-07-17"),
        )
        .unwrap();
        assert_eq!(due, Some(date("2026-07-20")));
    }

    #[test]
    fn unrecognized_rule_is_none() {
        assert!(
            compute_next_occurrence(
                "every blue moon",
                Some(date("2026-07-19")),
                None,
                None,
                date("2026-07-19")
            )
            .is_none()
        );
    }
}
