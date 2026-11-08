//! Record tracing events on the calling thread during tests.
//!
//! One process-wide subscriber is installed on the first [`Capture::arm`].
//! It answers `Interest::sometimes`, so a callsite an earlier test cached as
//! disabled is rebuilt, and each later event asks [`enabled`](tracing::Subscriber::enabled)
//! again. The event is kept only while this thread holds a guard. Other threads
//! drop it. The guard is not `Send`: an async test that captures logs must stay
//! on `current_thread`, or the buffer and the `await` land on different threads.
//!
//! `set_global_default` runs once. When another subscriber already owns the
//! process, that call fails. [`Capture::installed`] is then false, and this
//! guard records nothing.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::OnceLock;

use tracing::field::Visit;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

thread_local! {
    static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// Lines recorded on this thread until the guard drops.
pub struct Capture {
    installed: bool,
    _thread: PhantomData<*const ()>,
}

impl Capture {
    /// Start a fresh buffer on this thread and install the subscriber once.
    ///
    /// [`Self::installed`] reports whether this capture can record events.
    pub fn arm() -> Self {
        let installed = install();
        LINES.with(|slot| *slot.borrow_mut() = Some(Vec::new()));
        Self {
            installed,
            _thread: PhantomData,
        }
    }

    /// True when this process uses the log-capture subscriber.
    pub fn installed(&self) -> bool {
        self.installed
    }

    /// Field lines joined by newlines, in record order.
    pub fn text(&self) -> String {
        LINES.with(|slot| match slot.borrow().as_ref() {
            Some(lines) => lines.join("\n"),
            None => String::new(),
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        LINES.with(|slot| *slot.borrow_mut() = None);
    }
}

/// False when a global subscriber was already set, so capture cannot record.
fn install() -> bool {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    *INSTALLED.get_or_init(
        || match tracing::subscriber::set_global_default(LogCapture) {
            Ok(()) => true,
            Err(_) => false,
        },
    )
}

struct LogCapture;

struct FieldVisit(Vec<String>);

impl Visit for FieldVisit {
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

impl Subscriber for LogCapture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        LINES.with(|slot| slot.borrow().is_some())
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::TRACE)
    }

    fn register_callsite(&self, _: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        // `sometimes` makes each event ask the current thread. `always` would
        // skip that check for every later test in the process.
        tracing::subscriber::Interest::sometimes()
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut visit = FieldVisit(Vec::new());
        event.record(&mut visit);
        let line = visit.0.join(" ");
        LINES.with(|slot| {
            if let Some(lines) = slot.borrow_mut().as_mut() {
                lines.push(line);
            }
        });
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[cfg(test)]
mod tests {
    use super::Capture;

    #[test]
    fn armed_thread_keeps_fields_and_a_later_arm_starts_empty() {
        let first = Capture::arm();
        assert!(first.installed());
        tracing::info!(row_id = 7_i64, due_at = %"2026-11-08T10:00:00Z", "reminder_enqueued");
        let text = first.text();
        assert!(text.contains("message=reminder_enqueued"), "{text}");
        assert!(text.contains("row_id=7"), "{text}");
        assert!(text.contains("due_at=2026-11-08T10:00:00Z"), "{text}");
        assert!(!text.contains("due_at=\""), "{text}");
        drop(first);

        tracing::info!(row_id = 8_i64, "reminder_enqueued");
        let second = Capture::arm();
        assert!(second.installed());
        assert_eq!(second.text(), "");
    }
}
