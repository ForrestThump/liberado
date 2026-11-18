//! Catch-up for mechanical reminder schedules (PLAN.md).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use liberado_common::UtcStamp;

use crate::MechanicalJob;
use crate::cursor::ScheduleCursorStore;
use crate::fire_at;

pub const MECHANICAL_REMINDER_JOBS: &[&str] = &[
    "task-ping",
    "event-ping",
    "habit-ping",
    "vault-reminder-tick",
];

pub struct CatchUpSettings {
    pub window: SignedDuration,
    pub store: Arc<dyn ScheduleCursorStore>,
    pub floor: Arc<Mutex<HashMap<String, UtcStamp>>>,
}

impl std::fmt::Debug for CatchUpSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatchUpSettings")
            .field("window", &self.window)
            .finish_non_exhaustive()
    }
}

pub fn mechanical_reminder(job: &Option<MechanicalJob>) -> bool {
    job.as_ref()
        .is_some_and(|row| MECHANICAL_REMINDER_JOBS.contains(&row.kind.as_str()))
}

/// Civil-time fires strictly after `cursor` and at or before `now`, inside `window`.
pub fn missed_fires_in_window(
    parsed: &jiff_cron::Schedule,
    zone: Option<&TimeZone>,
    cursor: UtcStamp,
    now: UtcStamp,
    window: SignedDuration,
) -> Vec<UtcStamp> {
    let mut out = Vec::new();
    let mut after = cursor;
    for _ in 0..512 {
        let Some(next) = fire_at::next_utc(parsed, zone, after) else {
            break;
        };
        if next > now {
            break;
        }
        if now.duration_since(next) > window {
            after = next;
            continue;
        }
        if next > cursor {
            out.push(next);
        }
        after = next;
    }
    out
}

pub fn pick_emit_fire(scheduled: UtcStamp, missed: &[UtcStamp], now: UtcStamp) -> (UtcStamp, bool) {
    if let Some(&latest) = missed.last()
        && latest <= now
    {
        return (latest, true);
    }
    (scheduled, false)
}

pub fn floor_blocks(floor: &Mutex<HashMap<String, UtcStamp>>, name: &str, fire: UtcStamp) -> bool {
    floor
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(name)
        .is_some_and(|seen| *seen == fire)
}

pub fn remember_floor(floor: &Mutex<HashMap<String, UtcStamp>>, name: &str, fire: UtcStamp) {
    floor
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(name.to_string(), fire);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    struct Mem {
        map: Mutex<HashMap<String, UtcStamp>>,
    }

    impl ScheduleCursorStore for Mem {
        fn ensure(&self, schedule: &str, now: UtcStamp) -> UtcStamp {
            let mut guard = self.map.lock().unwrap();
            if let Some(at) = guard.get(schedule) {
                return *at;
            }
            guard.insert(schedule.to_string(), now);
            now
        }
    }

    fn at(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> UtcStamp {
        UtcStamp::from_civil_utc(year, month, day, hour, minute, second).unwrap()
    }

    #[test]
    fn first_boot_ensure_sets_now_and_leaves_missed_empty() {
        let now = at(2026, 10, 29, 14, 5, 0);
        let store = Mem {
            map: Mutex::new(HashMap::new()),
        };
        let cursor = store.ensure("task-ping", now);
        assert_eq!(cursor, now);
        let parsed = jiff_cron::Schedule::from_str("0 0 * * * * *").unwrap();
        let missed =
            missed_fires_in_window(&parsed, None, cursor, now, SignedDuration::from_hours(6));
        assert!(missed.is_empty());
    }

    #[test]
    fn a_clock_jump_enqueues_the_missed_minute_once() {
        let cursor = at(2026, 10, 29, 13, 58, 0);
        let now = at(2026, 10, 29, 14, 5, 0);
        let parsed = jiff_cron::Schedule::from_str("0 0 14 * * * *").unwrap();
        let missed =
            missed_fires_in_window(&parsed, None, cursor, now, SignedDuration::from_hours(6));
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0], at(2026, 10, 29, 14, 0, 0));
        let (fire, catch_up) = pick_emit_fire(at(2026, 10, 29, 14, 0, 0), &missed, now);
        assert!(catch_up);
        assert_eq!(fire, missed[0]);
    }

    #[test]
    fn a_seven_hour_old_fire_is_outside_a_six_hour_window() {
        let cursor = at(2026, 10, 29, 6, 0, 0);
        let now = at(2026, 10, 29, 14, 5, 0);
        let parsed = jiff_cron::Schedule::from_str("0 0 7 * * * *").unwrap();
        let missed =
            missed_fires_in_window(&parsed, None, cursor, now, SignedDuration::from_hours(6));
        assert!(missed.is_empty());
    }

    #[test]
    fn the_in_memory_floor_blocks_a_second_emit_of_the_same_instant() {
        let floor = Arc::new(Mutex::new(HashMap::new()));
        let at = at(2026, 10, 29, 14, 0, 0);
        assert!(!floor_blocks(&floor, "job", at));
        remember_floor(&floor, "job", at);
        assert!(floor_blocks(&floor, "job", at));
    }
}
