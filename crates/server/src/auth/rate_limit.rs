//! Sliding-window rate limits for auth endpoints.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PER_IP_LIMIT: u32 = 5;
const GLOBAL_LIMIT: u32 = 30;
const WINDOW: Duration = Duration::from_secs(60);

#[derive(Default)]
struct LimiterState {
    per_ip: HashMap<String, Window>,
    global: Window,
}

#[derive(Default)]
struct Window {
    started: Option<Instant>,
    count: u32,
}

/// Rate limit failed auth attempts (token login, passkey finish, register finish).
pub struct AuthRateLimiter {
    state: Mutex<LimiterState>,
    now: fn() -> Instant,
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self {
            state: Mutex::new(LimiterState::default()),
            now: Instant::now,
        }
    }
}

impl AuthRateLimiter {
    #[cfg(test)]
    pub fn with_clock(now: fn() -> Instant) -> Self {
        Self {
            state: Mutex::new(LimiterState::default()),
            now,
        }
    }

    /// `true` when the attempt should be refused with 429.
    pub fn record_failure(&self, client_ip: &str) -> bool {
        let now = (self.now)();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        bump(&mut state.global, now);
        let entry = state.per_ip.entry(client_ip.to_string()).or_default();
        bump(entry, now);
        if entry.count > PER_IP_LIMIT || state.global.count > GLOBAL_LIMIT {
            return true;
        }
        false
    }

    pub fn retry_after_secs(&self) -> u64 {
        WINDOW.as_secs()
    }
}

fn bump(window: &mut Window, now: Instant) {
    if window
        .started
        .is_none_or(|s| now.duration_since(s) >= WINDOW)
    {
        window.started = Some(now);
        window.count = 0;
    }
    window.count += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixth_failure_from_one_ip_is_limited() {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        fn now() -> Instant {
            *T0.get_or_init(Instant::now)
        }
        let _ = now();
        let limiter = AuthRateLimiter::with_clock(now);
        for _ in 0..5 {
            assert!(!limiter.record_failure("203.0.113.1"));
        }
        assert!(limiter.record_failure("203.0.113.1"));
    }
}
