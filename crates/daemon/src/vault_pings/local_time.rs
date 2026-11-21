//! Local civil minute matching with DST rules (PLAN.md).

use jiff::civil::{Date, DateTime, Time};
use jiff::tz::{AmbiguousOffset, TimeZone};
use liberado_common::UtcStamp;

/// True when `date`+`time` is the operator's current local minute.
/// Spring gap (`None`): false. Autumn fold (`Ambiguous`): earlier offset only.
pub fn same_local_minute(zone: &TimeZone, date: Date, time: Time, now: DateTime) -> bool {
    if now.date() != date || now.hour() != time.hour() || now.minute() != time.minute() {
        return false;
    }
    let local = DateTime::from_parts(date, time);
    match zone.to_ambiguous_timestamp(local).offset() {
        AmbiguousOffset::Unambiguous { .. } => true,
        AmbiguousOffset::Fold { before, .. } => {
            let Ok(timestamp) = before.to_timestamp(local) else {
                return false;
            };
            let mapped = timestamp.to_zoned(zone.clone()).datetime();
            mapped.date() == date
                && mapped.hour() == time.hour()
                && mapped.minute() == time.minute()
                && mapped == now
        }
        AmbiguousOffset::Gap { .. } => false,
    }
}

/// Earlier instant for a local civil time. A spring gap returns `None`.
pub(super) fn resolve_local(zone: &TimeZone, local: DateTime) -> Option<UtcStamp> {
    let offset = match zone.to_ambiguous_timestamp(local).offset() {
        AmbiguousOffset::Unambiguous { offset } => offset,
        AmbiguousOffset::Fold { before, .. } => before,
        AmbiguousOffset::Gap { .. } => return None,
    };
    offset.to_timestamp(local).ok().map(UtcStamp::from)
}

pub(super) fn parse_zone(name: &str) -> TimeZone {
    TimeZone::get(name).unwrap_or(TimeZone::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chicago_spring_gap_skips_the_missing_hour() {
        let zone = TimeZone::get("America/Chicago").unwrap();
        let date = Date::new(2026, 3, 8).unwrap();
        let missing = Time::new(2, 30, 0, 0).unwrap();
        let now = DateTime::from_parts(date, missing);
        assert!(!same_local_minute(&zone, date, missing, now));
        let ok = Time::new(3, 0, 0, 0).unwrap();
        let now = DateTime::from_parts(date, ok);
        assert!(same_local_minute(&zone, date, ok, now));
    }

    #[test]
    fn chicago_autumn_fold_uses_the_earlier_offset_once() {
        let zone = TimeZone::get("America/Chicago").unwrap();
        let date = Date::new(2026, 11, 1).unwrap();
        let folded = Time::new(1, 30, 0, 0).unwrap();
        let first = UtcStamp::from_civil_utc(2026, 11, 1, 6, 30, 0)
            .unwrap()
            .timestamp()
            .to_zoned(zone.clone())
            .datetime();
        assert!(same_local_minute(&zone, date, folded, first));
    }
}
