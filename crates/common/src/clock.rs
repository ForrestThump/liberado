//! A test-friendly clock so time-dependent code (TTL expiry, budget exhaustion, TTFT)
//! can be controlled deterministically in tests.
//!
//! Call [`now`] everywhere you would call [`std::time::Instant::now`]. In production it
//! passes straight through; tests call [`test_freeze_at`] / [`test_thaw`] to control it.

use std::sync::LazyLock;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static FROZEN: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// Freeze the clock at the given instant. All subsequent [`now`] calls return this value.
pub fn test_freeze_at(at: Instant) {
    *FROZEN.lock().unwrap() = Some(at);
}

/// Resume normal wall-clock time.
pub fn test_thaw() {
    *FROZEN.lock().unwrap() = None;
}

/// Advance the frozen clock by `d` (only effective when frozen).
pub fn test_advance(d: Duration) {
    if let Some(ref mut at) = *FROZEN.lock().unwrap() {
        *at += d;
    }
}

/// A deterministic source of the current time. In production this is [`Instant::now`]; in tests it
/// can be frozen via [`test_freeze_at`] / [`test_thaw`].
pub fn now() -> Instant {
    if let Some(at) = *FROZEN.lock().unwrap() {
        return at;
    }
    Instant::now()
}
