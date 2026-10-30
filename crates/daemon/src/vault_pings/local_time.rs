//! Local civil minute matching with DST rules (PLAN.md).

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use chrono::TimeZone;
use chrono_tz::Tz;

/// True when `date`+`time` is the operator's current local minute.
/// Spring gap (`None`): false. Autumn fold (`Ambiguous`): earlier offset only.
pub fn same_local_minute(zone: Tz, date: NaiveDate, time: NaiveTime, now: NaiveDateTime) -> bool {
    if now.date() != date || now.time().hour() != time.hour() || now.time().minute() != time.minute() {
        return false;
    }
    let local = NaiveDateTime::new(date, time);
    match zone.from_local_datetime(&local) {
        chrono::LocalResult::Single(_) => true,
        chrono::LocalResult::Ambiguous(first, _second) => {
            let mapped = first.with_timezone(&zone).naive_local();
            mapped.date() == date
                && mapped.time().hour() == time.hour()
                && mapped.time().minute() == time.minute()
                && mapped == now
        }
        chrono::LocalResult::None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn chicago_spring_gap_skips_the_missing_hour() {
        let zone: Tz = "America/Chicago".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let missing = NaiveTime::from_hms_opt(2, 30, 0).unwrap();
        let now = NaiveDateTime::new(date, missing);
        assert!(!same_local_minute(zone, date, missing, now));
        let ok = NaiveTime::from_hms_opt(3, 0, 0).unwrap();
        let now = NaiveDateTime::new(date, ok);
        assert!(same_local_minute(zone, date, ok, now));
    }

    #[test]
    fn chicago_autumn_fold_uses_the_earlier_offset_once() {
        let zone: Tz = "America/Chicago".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let folded = NaiveTime::from_hms_opt(1, 30, 0).unwrap();
        let first = chrono::Utc
            .with_ymd_and_hms(2026, 11, 1, 6, 30, 0)
            .unwrap()
            .with_timezone(&zone)
            .naive_local();
        assert!(same_local_minute(zone, date, folded, first));
    }
}
