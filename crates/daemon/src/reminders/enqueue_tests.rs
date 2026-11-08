//! Tests for mechanical reminder enqueue fan-out.

use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use liberado_config::RemindersConfig;
use liberado_outbox::{EnqueueOutcome, NewReminder, Outbox, OutboxSource, key_hash_suffix};

use super::delivery::OutboxDelivery;
use super::enqueue::log_reminder_enqueue;
use super::{MechanicalReminder, enqueue_mechanical};

/// Serializes reminder log lines so one test cannot freeze a callsite as disabled.
pub(crate) mod log_interest {
    use std::cell::Cell;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    thread_local! {
        static CAPTURE: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn set_capture(on: bool) {
        CAPTURE.with(|flag| flag.set(on));
    }

    pub(crate) fn hold() -> MutexGuard<'static, ()> {
        let guard = LOCK.lock().expect("log interest lock");
        if CAPTURE.with(|flag| flag.get()) {
            tracing::callsite::rebuild_interest_cache();
        }
        guard
    }
}

struct ClearCapture;

impl Drop for ClearCapture {
    fn drop(&mut self) {
        log_interest::set_capture(false);
    }
}

#[derive(Clone)]
struct LogCapture {
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
    fn new() -> Self {
        Self {
            lines: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn text(&self) -> String {
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
        // `sometimes` makes each event ask the current thread's subscriber.
        // `always` would skip that check for every later test in the process.
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

fn capture(f: impl FnOnce()) -> String {
    let logs = LogCapture::new();
    log_interest::set_capture(true);
    let _clear = ClearCapture;
    tracing::subscriber::with_default(logs.clone(), f);
    logs.text()
}

fn open_outbox() -> Arc<Outbox> {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    Arc::new(Outbox::open(&path).unwrap())
}

#[test]
fn enqueue_mechanical_writes_telegram_row_in_fallback_mode() {
    let outbox = open_outbox();
    let delivery = OutboxDelivery::new(Arc::clone(&outbox), None, None);
    let mut config = RemindersConfig::default();
    config.channels.telegram.mode = "fallback".into();
    let now = Utc::now();
    let item = MechanicalReminder {
        idempotency_key: "job/task-ping/2026-01-01T00:00:00Z".into(),
        title: "tasks".into(),
        body: "Do the thing".into(),
        due_at: now,
        catch_up: false,
        kind: liberado_outbox::TASK_LIST.into(),
        source: None,
    };
    enqueue_mechanical(
        &outbox,
        &config,
        &delivery,
        true,
        std::slice::from_ref(&item),
        now,
    )
    .expect("enqueue");
    let counts = outbox.counts(now).expect("counts");
    assert_eq!(counts.pending, 1);
}

fn secret_row(due_at: chrono::DateTime<Utc>) -> NewReminder {
    NewReminder {
        idempotency_key: "remind/task/Life/SECRETPATH.md/due/2026-11-08T10:00/abc/webpush/sub:7"
            .into(),
        channel: "webpush".into(),
        target: "sub:7".into(),
        title: "SECRET_TITLE_DO_NOT_LOG".into(),
        body: "SECRET_BODY_DO_NOT_LOG".into(),
        due_at,
        catch_up: false,
        kind: Some("task_due".into()),
        source: Some(OutboxSource::VaultNote {
            path: "Life/SECRETPATH.md".into(),
            label: "due".into(),
            text: "SECRET_TASK_TEXT".into(),
        }),
    }
}

#[test]
fn enqueue_log_records_ids_and_hides_the_reminder() {
    let due = Utc.with_ymd_and_hms(2026, 11, 8, 10, 0, 0).unwrap();
    let row = secret_row(due);
    let hash = key_hash_suffix(&row.idempotency_key);
    let inserted = capture(|| log_reminder_enqueue(&row, EnqueueOutcome::Inserted(42)));
    assert!(inserted.contains("reminder_enqueued"), "{inserted}");
    assert!(inserted.contains("row_id=42"), "{inserted}");
    assert!(inserted.contains("subscription_id=7"), "{inserted}");
    assert!(inserted.contains("kind=task_due"), "{inserted}");
    assert!(
        inserted.contains("due_at=2026-11-08T10:00:00Z"),
        "{inserted}"
    );
    assert!(inserted.contains(&format!("key_hash={hash}")), "{inserted}");
    assert!(!inserted.contains("SECRET"), "{inserted}");
    assert!(!inserted.contains("sub:7"), "{inserted}");

    let skipped = capture(|| log_reminder_enqueue(&row, EnqueueOutcome::Duplicate));
    assert!(skipped.contains("reminder_enqueue_skipped"), "{skipped}");
    assert!(skipped.contains("reason=duplicate"), "{skipped}");
    assert!(skipped.contains(&format!("key_hash={hash}")), "{skipped}");
    assert!(skipped.contains("subscription_id=7"), "{skipped}");
    assert!(!skipped.contains("SECRET"), "{skipped}");
    assert!(!skipped.contains("row_id="), "{skipped}");
}

#[test]
fn enqueue_mechanical_logs_insert_and_duplicate_without_reminder_text() {
    let outbox = open_outbox();
    let delivery = OutboxDelivery::new(Arc::clone(&outbox), None, None);
    let mut config = RemindersConfig::default();
    config.channels.telegram.mode = "fallback".into();
    let now = Utc.with_ymd_and_hms(2026, 11, 8, 10, 0, 0).unwrap();
    let item = MechanicalReminder {
        idempotency_key: "remind/task/Life/SECRETPATH.md/due/2026-11-08T10:00/abc".into(),
        title: "SECRET_TITLE_DO_NOT_LOG".into(),
        body: "SECRET_BODY_DO_NOT_LOG".into(),
        due_at: now,
        catch_up: false,
        kind: liberado_outbox::TASK_DUE.into(),
        source: None,
    };
    let text = capture(|| {
        enqueue_mechanical(
            &outbox,
            &config,
            &delivery,
            true,
            std::slice::from_ref(&item),
            now,
        )
        .expect("insert");
        enqueue_mechanical(
            &outbox,
            &config,
            &delivery,
            true,
            std::slice::from_ref(&item),
            now,
        )
        .expect("duplicate");
    });
    assert!(text.contains("reminder_enqueued"), "{text}");
    assert!(text.contains("reminder_enqueue_skipped"), "{text}");
    assert!(text.contains("reason=duplicate"), "{text}");
    assert!(text.contains("due_at=2026-11-08T10:00:00Z"), "{text}");
    assert!(!text.contains("SECRET"), "{text}");
    assert!(!text.contains("subscription_id"), "{text}");
}

#[test]
fn enqueue_mechanical_with_no_rows_does_not_log_enqueue() {
    let outbox = open_outbox();
    let delivery = OutboxDelivery::new(Arc::clone(&outbox), None, None);
    let config = RemindersConfig::default();
    let text = capture(|| {
        enqueue_mechanical(&outbox, &config, &delivery, false, &[], Utc::now()).expect("empty");
    });
    assert!(!text.contains("reminder_enqueued"), "{text}");
    assert!(!text.contains("reminder_enqueue_skipped"), "{text}");
}
