//! A test-friendly clock so time-dependent code (TTL expiry, budget exhaustion, TTFT)
//! can be controlled deterministically in tests.
//!
//! Call [`now`] everywhere you would call [`std::time::Instant::now`]. In production it
//! passes straight through; in `#[cfg(test)]` it respects a thread-local freeze set by tests.

use std::time::Instant;

/// State that is only present in test builds.
#[cfg(test)]
mod frozen {
    use super::*;
    use std::sync::Mutex;
    use std::sync::LazyLock;

    static FROZEN: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

    /// Freeze the clock at the given instant. All subsequent [`super::now`] calls return this value.
    pub fn freeze_at(at: Instant) {
        *FROZEN.lock().unwrap() = Some(at);
    }

    /// Resume normal wall-clock time.
    pub fn thaw() {
        *FROZEN.lock().unwrap() = None;
    }

    /// Advance the frozen clock by `d` (only effective when frozen).
    pub fn advance(d: std::time::Duration) {
        if let Some(ref mut at) = *FROZEN.lock().unwrap() {
            *at += d;
        }
    }

    pub fn now_inner() -> Option<Instant> {
        *FROZEN.lock().unwrap()
    }
}

/// A deterministic source of the current time. In production this is [`Instant::now`]; in tests it
/// can be frozen via [`test_freeze_at`] / [`test_thaw`].
pub fn now() -> Instant {
    #[cfg(test)]
    {
        if let Some(at) = frozen::now_inner() {
            return at;
        }
    }
    Instant::now()
}

/// Freeze time at the given instant. Only available in `#[cfg(test)]` builds.
#[cfg(test)]
pub fn test_freeze_at(at: Instant) {
    frozen::freeze_at(at);
}

/// Resume normal wall-clock time. Only available in `#[cfg(test)]` builds.
#[cfg(test)]
pub fn test_thaw() {
    frozen::thaw();
}

/// Advance the frozen clock by `d`. Only available in `#[cfg(test)]` builds.
#[cfg(test)]
pub fn test_advance(d: std::time::Duration) {
    frozen::advance(d);
}
