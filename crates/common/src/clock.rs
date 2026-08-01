//! A test-friendly clock so time-dependent code (TTL expiry, budget exhaustion, TTFT)
//! can be controlled deterministically in tests.
//!
//! Call [`now`] everywhere you would call [`std::time::Instant::now`].
//!
//! # Why the controls are gated, and guard-shaped
//!
//! Freezing is **compiled out** of ordinary builds. Under `cfg(test)` (this crate's own tests) or
//! the `test-clock` feature (other crates' tests, via a dev-dependency) `now` consults a frozen
//! instant; otherwise it is a straight `Instant::now()` — no lock, no branch. Two reasons:
//!
//! * `liberado-common` is a foundation crate, so everything depends on it. "Freeze the global
//!   clock" is not a control any production caller should be able to reach, and TTL expiry and
//!   budget exhaustion are precisely the things that must not be freezable at runtime.
//! * A mutex on every time read is a real cost paid solely for test control.
//!
//! Freezing returns a [`FrozenClock`] guard that thaws on drop, and there is no unguarded way to
//! freeze. That is not tidiness. The clock is process-global and Rust's harness keeps running the
//! remaining tests in the same process after one fails, so a bare `freeze(); …; thaw();` leaks a
//! frozen clock to every later test the moment an assertion between the two fails. Demonstrated
//! rather than theorised: a deliberately failing test left the clock frozen, and the next test in
//! the binary saw time stand still. `Drop` runs while unwinding, so the guard holds even when the
//! assertion does not.

use std::time::Instant;

#[cfg(any(test, feature = "test-clock"))]
mod controllable {
    use std::sync::{LazyLock, Mutex, MutexGuard};
    use std::time::{Duration, Instant};

    static FROZEN: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

    /// Poison-tolerant access. A test that panics while holding this lock would otherwise poison it
    /// and turn every later `now()` — including calls inside the production code under test — into
    /// a panic, converting one failure into a cascade that buries its own cause.
    fn frozen() -> MutexGuard<'static, Option<Instant>> {
        FROZEN.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Freeze the clock at `at` until the returned guard drops.
    ///
    /// ```ignore
    /// let clock = liberado_common::clock::test_freeze_at(Instant::now());
    /// clock.advance(Duration::from_secs(5));
    /// // thaws here — including on the panic path
    /// ```
    #[must_use = "the clock thaws when this guard drops; binding to `_` thaws immediately"]
    pub fn test_freeze_at(at: Instant) -> FrozenClock {
        *frozen() = Some(at);
        FrozenClock { _private: () }
    }

    /// Holds the clock frozen. Thaws on drop, including while unwinding from a failed assertion.
    pub struct FrozenClock {
        _private: (),
    }

    impl FrozenClock {
        /// Advance the frozen instant by `d`.
        pub fn advance(&self, d: Duration) {
            if let Some(at) = frozen().as_mut() {
                *at += d;
            }
        }
    }

    impl Drop for FrozenClock {
        fn drop(&mut self) {
            *frozen() = None;
        }
    }

    /// Advance the frozen instant by `d`, from anywhere. No-op when the clock is not frozen.
    ///
    /// A free function as well as [`FrozenClock::advance`] because time often has to move from
    /// *inside* the code under test — a provider double standing in for a slow call — where the
    /// guard is not in scope. This cannot leak state: only freezing can, and that still requires
    /// the guard.
    pub fn test_advance(d: Duration) {
        if let Some(at) = frozen().as_mut() {
            *at += d;
        }
    }

    /// The frozen instant, if one is held. See [`super::now`].
    pub(super) fn frozen_now() -> Option<Instant> {
        *frozen()
    }
}

#[cfg(any(test, feature = "test-clock"))]
pub use controllable::{FrozenClock, test_advance, test_freeze_at};

/// A deterministic source of the current time.
///
/// Under `cfg(test)` or the `test-clock` feature this returns the frozen instant while a
/// [`FrozenClock`] is held, and the wall clock otherwise.
#[cfg(any(test, feature = "test-clock"))]
pub fn now() -> Instant {
    controllable::frozen_now().unwrap_or_else(Instant::now)
}

/// A deterministic source of the current time — the ordinary build: exactly [`Instant::now`].
#[cfg(not(any(test, feature = "test-clock")))]
#[inline]
pub fn now() -> Instant {
    Instant::now()
}
