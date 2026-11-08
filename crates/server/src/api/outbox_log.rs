//! Structured logs for outbox actions. Ids and timestamps only.

use chrono::{DateTime, SecondsFormat, Utc};
use tracing::{info, warn};

pub(crate) const VIA_DETAIL: &str = "detail";
pub(crate) const VIA_TRAY: &str = "tray";

pub(crate) fn log_outbox_action(
    action: &str,
    via: &str,
    row_id: i64,
    received_at: DateTime<Utc>,
    result: &str,
) {
    let received_at = rfc3339(received_at);
    #[cfg(test)]
    let hold = capture::hold();
    if result == "error" {
        warn!(
            action,
            via,
            row_id,
            received_at = %received_at,
            result,
            "outbox_action"
        );
    } else {
        info!(
            action,
            via,
            row_id,
            received_at = %received_at,
            result,
            "outbox_action"
        );
    }
    #[cfg(test)]
    drop(hold);
}

pub(crate) struct SnoozeApplied<'a> {
    pub via: &'a str,
    pub row_id: i64,
    pub minutes: i64,
    pub received_at: DateTime<Utc>,
    pub prior_due_at: DateTime<Utc>,
    pub new_due_at: DateTime<Utc>,
    pub follow_up_row_id: i64,
}

pub(crate) fn log_snooze_applied(entry: &SnoozeApplied<'_>) {
    let received_at = rfc3339(entry.received_at);
    let prior_due_at = rfc3339(entry.prior_due_at);
    let new_due_at = rfc3339(entry.new_due_at);
    #[cfg(test)]
    let hold = capture::hold();
    info!(
        action = "snooze",
        via = entry.via,
        row_id = entry.row_id,
        minutes = entry.minutes,
        received_at = %received_at,
        result = "applied",
        prior_due_at = %prior_due_at,
        new_due_at = %new_due_at,
        follow_up_row_id = entry.follow_up_row_id,
        "outbox_action"
    );
    #[cfg(test)]
    drop(hold);
}

pub(crate) fn log_snooze_stopped(
    via: &str,
    row_id: i64,
    minutes: i64,
    received_at: DateTime<Utc>,
    result: &str,
) {
    let received_at = rfc3339(received_at);
    #[cfg(test)]
    let hold = capture::hold();
    if result == "error" {
        warn!(
            action = "snooze",
            via,
            row_id,
            minutes,
            received_at = %received_at,
            result,
            "outbox_action"
        );
    } else {
        info!(
            action = "snooze",
            via,
            row_id,
            minutes,
            received_at = %received_at,
            result,
            "outbox_action"
        );
    }
    #[cfg(test)]
    drop(hold);
}

pub(crate) fn log_body_tap_ack(row_id: i64, received_at: DateTime<Utc>) {
    let received_at = rfc3339(received_at);
    #[cfg(test)]
    let hold = capture::hold();
    info!(
        row_id,
        received_at = %received_at,
        ack = "body_tap",
        "outbox_ack"
    );
    #[cfg(test)]
    drop(hold);
}

pub(crate) fn log_ack_failed(row_id: i64) {
    #[cfg(test)]
    let hold = capture::hold();
    warn!(row_id, "outbox_ack failed");
    #[cfg(test)]
    drop(hold);
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
pub(crate) mod capture {
    use std::cell::Cell;
    use std::sync::{Arc, Mutex, MutexGuard};

    static INTEREST_LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        static CAPTURE: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn hold() -> MutexGuard<'static, ()> {
        let guard = INTEREST_LOCK.lock().expect("log interest lock");
        if CAPTURE.with(|flag| flag.get()) {
            tracing::callsite::rebuild_interest_cache();
        }
        guard
    }

    pub(crate) struct Session {
        logs: LogCapture,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl Session {
        pub(crate) fn arm(logs: LogCapture) -> Self {
            CAPTURE.with(|flag| flag.set(true));
            let guard = tracing::subscriber::set_default(logs.clone());
            Self {
                logs,
                _guard: guard,
            }
        }

        pub(crate) fn text(&self) -> String {
            self.logs.text()
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            CAPTURE.with(|flag| flag.set(false));
        }
    }

    #[derive(Clone)]
    pub(crate) struct LogCapture {
        lines: Arc<Mutex<Vec<(tracing::Level, String)>>>,
    }

    struct FieldVisit(Vec<String>);

    impl tracing::field::Visit for FieldVisit {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            self.0.push(format!("{}={value}", field.name()));
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push(format!("{}={value:?}", field.name()));
        }
    }

    impl LogCapture {
        pub(crate) fn new() -> Self {
            Self {
                lines: Arc::new(Mutex::new(Vec::new())),
            }
        }

        pub(crate) fn text(&self) -> String {
            self.lines
                .lock()
                .expect("log lock")
                .iter()
                .map(|(_, line)| line.clone())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    impl tracing::Subscriber for LogCapture {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::sometimes()
        }

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visit = FieldVisit(Vec::new());
            event.record(&mut visit);
            self.lines
                .lock()
                .expect("log lock")
                .push((*event.metadata().level(), visit.0.join(" ")));
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }
}
