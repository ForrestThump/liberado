//! Tests for mechanical reminder enqueue fan-out.

use std::sync::Arc;

use chrono::Utc;
use liberado_config::RemindersConfig;
use liberado_outbox::Outbox;

use super::delivery::OutboxDelivery;
use super::{MechanicalReminder, enqueue_mechanical};

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
