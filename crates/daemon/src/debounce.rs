//! Per-path event debouncing.
//!
//! A single filesystem write produces a *burst* of `notify` events (on Windows, typically
//! Create + Modify + Modify). Reacting to each would fire duplicate reactions. The [`Debouncer`]
//! coalesces a burst per path into a single processing trigger once the path has been quiet for a
//! configured period — which also means we attribute the *settled* content, not an intermediate
//! state. It is the low-timescale floor of the inbox spec's settle window (same mechanism, much
//! shorter duration).
//!
//! It is pure and clock-injectable (`now` is passed in), so the coalescing logic is tested
//! deterministically without a runtime or real time.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Coalesces bursts of activity per path. Holds a deadline per pending path; a path becomes
/// "ready" once `now` reaches its deadline with no further activity.
pub(crate) struct Debouncer {
    quiet: Duration,
    deadlines: HashMap<PathBuf, Instant>,
}

impl Debouncer {
    pub(crate) fn new(quiet: Duration) -> Self {
        Self {
            quiet,
            deadlines: HashMap::new(),
        }
    }

    /// Record activity on `path` observed at `now`, (re)setting its deadline to `now + quiet`.
    /// Repeated calls within the quiet period keep pushing the deadline out — that is the
    /// coalescing.
    pub(crate) fn observe(&mut self, path: PathBuf, now: Instant) {
        self.deadlines.insert(path, now + self.quiet);
    }

    /// Remove and return every path whose quiet period has elapsed by `now`.
    pub(crate) fn drain_ready(&mut self, now: Instant) -> Vec<PathBuf> {
        let ready: Vec<PathBuf> = self
            .deadlines
            .iter()
            .filter(|&(_, &deadline)| deadline <= now)
            .map(|(path, _)| path.clone())
            .collect();
        for path in &ready {
            self.deadlines.remove(path);
        }
        ready
    }

    /// The earliest pending deadline, if any — used to schedule the next wakeup.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.deadlines.values().min().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_a_burst_into_one_ready_path() {
        let mut d = Debouncer::new(Duration::from_millis(100));
        let t0 = Instant::now();

        // A burst of events for the same path within the quiet window.
        d.observe("a.md".into(), t0);
        d.observe("a.md".into(), t0 + Duration::from_millis(20));
        d.observe("a.md".into(), t0 + Duration::from_millis(40));

        // The deadline tracks the *last* observation (t0+40 + 100 = t0+140).
        assert!(d.drain_ready(t0 + Duration::from_millis(139)).is_empty());
        assert_eq!(
            d.drain_ready(t0 + Duration::from_millis(140)),
            vec![PathBuf::from("a.md")]
        );

        // Drained exactly once; nothing remains.
        assert!(d.drain_ready(t0 + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn tracks_independent_paths_with_earliest_next_deadline() {
        let mut d = Debouncer::new(Duration::from_millis(100));
        let t0 = Instant::now();

        d.observe("a.md".into(), t0); // deadline t0+100
        d.observe("b.md".into(), t0 + Duration::from_millis(50)); // deadline t0+150

        assert_eq!(d.next_deadline(), Some(t0 + Duration::from_millis(100)));

        // Only `a` is ready at t0+100; `b` is still pending.
        assert_eq!(
            d.drain_ready(t0 + Duration::from_millis(100)),
            vec![PathBuf::from("a.md")]
        );
        assert_eq!(d.next_deadline(), Some(t0 + Duration::from_millis(150)));
    }

    #[test]
    fn empty_debouncer_has_no_deadline() {
        let d = Debouncer::new(Duration::from_millis(100));
        assert_eq!(d.next_deadline(), None);
    }

    #[test]
    fn zero_quiet_time_drains_immediately() {
        let mut d = Debouncer::new(Duration::ZERO);
        let t0 = Instant::now();
        d.observe("z.md".into(), t0);
        // With zero quiet time, deadline = t0, so drain_ready(t0) picks it up.
        assert_eq!(d.drain_ready(t0), vec![PathBuf::from("z.md")]);
    }

    #[test]
    fn large_quiet_time_does_not_overflow() {
        let mut d = Debouncer::new(Duration::from_secs(100_000_000));
        let t0 = Instant::now();
        d.observe("big.md".into(), t0);
        assert!(d.next_deadline().unwrap() > t0);
        // Should not panic with no cases ready.
        assert!(d.drain_ready(t0).is_empty());
    }
}
