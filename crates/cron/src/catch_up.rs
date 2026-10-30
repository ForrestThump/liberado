//! Catch-up for mechanical reminder schedules (PLAN.md).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};

use crate::cursor::ScheduleCursorStore;
use crate::fire_at;
use crate::MechanicalJob;

pub const MECHANICAL_REMINDER_JOBS: &[&str] = &[
    "task-ping",
    "event-ping",
    "habit-ping",
    "vault-reminder-tick",
];

pub struct CatchUpSettings {
    pub window: Duration,
    pub store: Arc<dyn ScheduleCursorStore>,
    pub floor: Arc<Mutex<HashMap<String, DateTime<Utc>>>>,
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
    parsed: &cron::Schedule,
    zone: Option<chrono_tz::Tz>,
    cursor: DateTime<Utc>,
    now: DateTime<Utc>,
    window: Duration,
) -> Vec<DateTime<Utc>> {
    let mut out = Vec::new();
    let mut after = cursor;
    for _ in 0..512 {
        let Some(next) = fire_at::next_utc(parsed, zone, after) else {
            break;
        };
        if next > now {
            break;
        }
        if now.signed_duration_since(next) > window {
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

pub fn pick_emit_fire(
    scheduled: DateTime<Utc>,
    missed: &[DateTime<Utc>],
    now: DateTime<Utc>,
) -> (DateTime<Utc>, bool) {
    if let Some(&latest) = missed.last() {
        if latest <= now {
            return (latest, true);
        }
    }
    (scheduled, false)
}

pub fn floor_blocks(floor: &Mutex<HashMap<String, DateTime<Utc>>>, name: &str, fire: DateTime<Utc>) -> bool {
    floor
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(name)
        .is_some_and(|seen| *seen == fire)
}

pub fn remember_floor(floor: &Mutex<HashMap<String, DateTime<Utc>>>, name: &str, fire: DateTime<Utc>) {
    floor
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .insert(name.to_string(), fire);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::str::FromStr;

    struct Mem {
        map: Mutex<HashMap<String, DateTime<Utc>>>,
    }

    impl ScheduleCursorStore for Mem {
        fn ensure(&self, schedule: &str, now: DateTime<Utc>) -> DateTime<Utc> {
            let mut guard = self.map.lock().unwrap();
            if let Some(at) = guard.get(schedule) {
                return *at;
            }
            guard.insert(schedule.to_string(), now);
            now
        }
    }

    #[test]
    fn first_boot_ensure_sets_now_and_leaves_missed_empty() {
        let now = Utc.with_ymd_and_hms(2026, 10, 29, 14, 5, 0).unwrap();
        let store = Mem {
            map: Mutex::new(HashMap::new()),
        };
        let cursor = store.ensure("task-ping", now);
        assert_eq!(cursor, now);
        let parsed = cron::Schedule::from_str("0 0 * * * * *").unwrap();
        let missed = missed_fires_in_window(&parsed, None, cursor, now, Duration::hours(6));
        assert!(missed.is_empty());
    }

    #[test]
    fn a_clock_jump_enqueues_the_missed_minute_once() {
        let cursor = Utc.with_ymd_and_hms(2026, 10, 29, 13, 58, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 29, 14, 5, 0).unwrap();
        let parsed = cron::Schedule::from_str("0 0 14 * * * *").unwrap();
        let missed = missed_fires_in_window(&parsed, None, cursor, now, Duration::hours(6));
        assert_eq!(missed.len(), 1);
        assert_eq!(
            missed[0],
            Utc.with_ymd_and_hms(2026, 10, 29, 14, 0, 0).unwrap()
        );
        let (fire, catch_up) = pick_emit_fire(
            Utc.with_ymd_and_hms(2026, 10, 29, 14, 0, 0).unwrap(),
            &missed,
            now,
        );
        assert!(catch_up);
        assert_eq!(fire, missed[0]);
    }

    #[test]
    fn a_seven_hour_old_fire_is_outside_a_six_hour_window() {
        let cursor = Utc.with_ymd_and_hms(2026, 10, 29, 6, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 29, 14, 5, 0).unwrap();
        let parsed = cron::Schedule::from_str("0 0 7 * * * *").unwrap();
        let missed = missed_fires_in_window(&parsed, None, cursor, now, Duration::hours(6));
        assert!(missed.is_empty());
    }

    #[test]
    fn the_in_memory_floor_blocks_a_second_emit_of_the_same_instant() {
        let floor = Arc::new(Mutex::new(HashMap::new()));
        let at = Utc.with_ymd_and_hms(2026, 10, 29, 14, 0, 0).unwrap();
        assert!(!floor_blocks(&floor, "job", at));
        remember_floor(&floor, "job", at);
        assert!(floor_blocks(&floor, "job", at));
    }
}
