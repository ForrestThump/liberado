//! Capped exponential backoff for MCP reconnect.
//!
//! The unjittered schedule is `min(cap, base * 2^(attempt-1))` with attempt starting at 1:
//! 1s, 2s, 4s, … up to 60s with the production constants. Jitter lands in `[delay/2, delay]`
//! so a retry never waits longer than the capped step and never collapses to zero.
//!
//! Logging uses the unjittered step. The first failure is logged. Later failures are logged
//! only when that step changes, including the transition onto the cap. Further waits at the
//! cap stay quiet.

use std::time::Duration;

/// Production base delay. The first failure waits about this long.
pub const BASE_DELAY: Duration = Duration::from_secs(1);
/// Production cap. The wait never grows past this.
pub const CAP_DELAY: Duration = Duration::from_secs(60);
/// How often a connected peer is probed with `list_tools`.
pub const PROBE_EVERY: Duration = Duration::from_secs(15);

/// What to log for one failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureLog {
    /// Attempt 1. Callers emit the historical warn line.
    First,
    /// The unjittered step changed (doubled, or landed on the cap).
    StillDown,
    /// Same step as the previous logged failure. Stay quiet.
    Silent,
}

/// One recorded failure: how to log it, and how long to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailureDecision {
    pub kind: FailureLog,
    pub attempt: u32,
    pub delay: Duration,
}

/// Failure count for the current outage. Reset after a successful connect.
#[derive(Debug, Default, Clone)]
pub struct BackoffCursor {
    attempt: u32,
    last_logged_step: Option<Duration>,
}

impl BackoffCursor {
    /// Connect tries this outage, including the next success. `1` when nothing has failed yet.
    pub fn success_attempts(&self) -> u32 {
        self.attempt.saturating_add(1)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record one failure. `unit` is the jitter draw in `[0, 1]`.
    pub fn record(&mut self, base: Duration, cap: Duration, unit: f64) -> FailureDecision {
        self.attempt = self.attempt.saturating_add(1);
        let step = capped_delay(base, cap, self.attempt);
        let kind = log_kind(self.attempt, step, self.last_logged_step);
        if kind != FailureLog::Silent {
            self.last_logged_step = Some(step);
        }
        FailureDecision {
            kind,
            attempt: self.attempt,
            delay: apply_jitter(step, unit),
        }
    }
}

/// Unjittered delay for a 1-based attempt. Attempt 0 is treated as the first step.
pub fn capped_delay(base: Duration, cap: Duration, attempt: u32) -> Duration {
    if base.is_zero() || cap.is_zero() {
        return Duration::ZERO;
    }
    let shift = attempt.saturating_sub(1).min(20);
    let grown = base.saturating_mul(1u32 << shift);
    if grown > cap { cap } else { grown }
}

/// Map `unit` into `[delay/2, delay]`. Non-finite or out-of-range units clamp.
pub fn apply_jitter(delay: Duration, unit: f64) -> Duration {
    let unit = finite_unit(unit);
    let half = delay / 2;
    let extra_ns = (half.as_nanos() as f64) * unit;
    let extra = Duration::from_nanos(extra_ns.min(u64::MAX as f64) as u64);
    half + extra
}

fn finite_unit(unit: f64) -> f64 {
    if unit.is_finite() {
        unit.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn log_kind(attempt: u32, step: Duration, last: Option<Duration>) -> FailureLog {
    if attempt == 1 {
        return FailureLog::First;
    }
    if last != Some(step) {
        return FailureLog::StillDown;
    }
    FailureLog::Silent
}

/// A wait of zero would busy-loop the peer task. Keep a minimum slice.
pub fn floor_delay(delay: Duration) -> Duration {
    if delay.is_zero() {
        Duration::from_millis(1)
    } else {
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Duration = Duration::from_secs(1);
    const CAP: Duration = Duration::from_secs(60);

    #[test]
    fn schedule_doubles_then_sticks_at_the_cap() {
        let expected = [1u64, 2, 4, 8, 16, 32, 60, 60, 60];
        for (idx, secs) in expected.iter().enumerate() {
            let attempt = (idx as u32) + 1;
            assert_eq!(
                capped_delay(BASE, CAP, attempt),
                Duration::from_secs(*secs),
                "attempt {attempt}"
            );
        }
    }

    #[test]
    fn jitter_stays_inside_half_and_full_delay() {
        let raw = Duration::from_secs(8);
        assert_eq!(apply_jitter(raw, 0.0), Duration::from_secs(4));
        assert_eq!(apply_jitter(raw, 0.5), Duration::from_secs(6));
        assert_eq!(apply_jitter(raw, 1.0), raw);
        assert_eq!(apply_jitter(raw, -3.0), Duration::from_secs(4));
        assert_eq!(apply_jitter(raw, 9.0), raw);
        assert_eq!(apply_jitter(raw, f64::NAN), raw);
        let capped = capped_delay(BASE, CAP, 20);
        let jittered = apply_jitter(capped, 0.25);
        assert!(jittered >= capped / 2 && jittered <= capped);
    }

    #[test]
    fn cursor_logs_first_failure_then_only_step_changes_and_resets() {
        let mut cursor = BackoffCursor::default();
        let first = cursor.record(BASE, CAP, 1.0);
        assert_eq!(first.kind, FailureLog::First);
        assert_eq!(first.attempt, 1);
        assert_eq!(first.delay, Duration::from_secs(1));
        assert_eq!(cursor.success_attempts(), 2);

        let second = cursor.record(BASE, CAP, 1.0);
        assert_eq!(second.kind, FailureLog::StillDown);
        assert_eq!(second.delay, Duration::from_secs(2));

        let mut saw_cap = false;
        let mut silent_after_cap = 0u32;
        for _ in 0..8 {
            let decision = cursor.record(BASE, CAP, 1.0);
            assert!(decision.delay <= CAP);
            if decision.delay == CAP && decision.kind == FailureLog::StillDown {
                saw_cap = true;
            }
            if saw_cap && decision.kind == FailureLog::Silent {
                silent_after_cap += 1;
                assert_eq!(decision.delay, CAP);
            }
        }
        assert!(saw_cap, "the climb must log once when it hits the cap");
        assert!(silent_after_cap >= 1, "further cap waits stay quiet");

        cursor.reset();
        assert_eq!(cursor.success_attempts(), 1);
        let again = cursor.record(BASE, CAP, 0.0);
        assert_eq!(again.kind, FailureLog::First);
        assert_eq!(again.attempt, 1);
        assert_eq!(again.delay, Duration::from_millis(500));
    }

    #[test]
    fn zero_base_is_zero_and_floor_keeps_a_slice() {
        assert_eq!(capped_delay(Duration::ZERO, CAP, 3), Duration::ZERO);
        assert_eq!(floor_delay(Duration::ZERO), Duration::from_millis(1));
        assert_eq!(floor_delay(Duration::from_secs(2)), Duration::from_secs(2));
    }
}
