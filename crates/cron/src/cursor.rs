//! Schedule fire cursor for mechanical reminder catch-up.

use chrono::{DateTime, Utc};

/// Read schedule cursors without pulling in the outbox crate.
pub trait ScheduleCursorStore: Send + Sync {
    /// On first sight, persist `now` and return it (no backfill window).
    fn ensure(&self, schedule: &str, now: DateTime<Utc>) -> DateTime<Utc>;
}
