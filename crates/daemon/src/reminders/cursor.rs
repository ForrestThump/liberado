//! Outbox-backed schedule cursor for cron catch-up.

use liberado_common::UtcStamp;
use liberado_cron::ScheduleCursorStore;
use liberado_outbox::Outbox;

pub struct OutboxScheduleCursor {
    outbox: std::sync::Arc<Outbox>,
}

impl OutboxScheduleCursor {
    pub fn new(outbox: std::sync::Arc<Outbox>) -> Self {
        Self { outbox }
    }
}

impl ScheduleCursorStore for OutboxScheduleCursor {
    fn ensure(&self, schedule: &str, now: UtcStamp) -> UtcStamp {
        self.outbox.ensure_cursor(schedule, now).unwrap_or(now)
    }
}
