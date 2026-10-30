use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use crate::batch::{DeliveryBatch, DeliveryOutcome};
use crate::pass::{DeliverySink, PassLimits, run_pass};
use crate::rows::{EnqueueOutcome, NewReminder};
use crate::store::Outbox;
use crate::subs::NewSubscription;

const UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";

#[cfg(test)]
struct Script {
    ready: Mutex<std::result::Result<(), String>>,
    outcomes: Mutex<Vec<DeliveryOutcome>>,
    calls: Mutex<u32>,
}

#[cfg(test)]
impl Script {
    fn ready(outcomes: Vec<DeliveryOutcome>) -> Self {
        Self {
            ready: Mutex::new(Ok(())),
            outcomes: Mutex::new(outcomes),
            calls: Mutex::new(0),
        }
    }

    fn held(message: &str) -> Self {
        Self {
            ready: Mutex::new(Err(message.into())),
            outcomes: Mutex::new(Vec::new()),
            calls: Mutex::new(0),
        }
    }

    fn calls(&self) -> u32 {
        *self.calls.lock().unwrap()
    }
}

#[cfg(test)]
#[async_trait]
impl DeliverySink for Script {
    fn readiness(&self, _channel: &str, _target: &str) -> std::result::Result<(), String> {
        self.ready.lock().unwrap().clone()
    }

    async fn deliver(&self, batch: &DeliveryBatch) -> DeliveryOutcome {
        *self.calls.lock().unwrap() += 1;
        assert!(!batch.body.contains("p256dh"));
        self.outcomes
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(DeliveryOutcome::Accepted)
    }
}

#[cfg(test)]
fn db() -> (Outbox, std::path::PathBuf) {
    let path = tempfile::NamedTempFile::new()
        .unwrap()
        .into_temp_path()
        .keep()
        .unwrap();
    let outbox = Outbox::open(&path).unwrap();
    (outbox, path)
}

#[cfg(test)]
fn reminder(key: &str, channel: &str, target: &str, catch_up: bool, due: DateTime<Utc>) -> NewReminder {
    NewReminder {
        idempotency_key: key.into(),
        channel: channel.into(),
        target: target.into(),
        title: "Reminder".into(),
        body: "call home".into(),
        due_at: due,
        catch_up,
    }
}

#[cfg(test)]
fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T15:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[tokio::test]
async fn telegram_failures_then_one_accepted_send() {
    let (outbox, _) = db();
    let at = now();
    outbox
        .enqueue(&reminder("k", "telegram", "reminder", false, at), at)
        .unwrap();
    let sink = Script::ready(vec![
        DeliveryOutcome::Accepted,
        DeliveryOutcome::Retryable("500".into()),
        DeliveryOutcome::Retryable("500".into()),
        DeliveryOutcome::Retryable("500".into()),
    ]);
    let mut at = at;
    for _ in 0..3 {
        run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
        at += Duration::hours(2);
    }
    assert_eq!(outbox.state_of(1).unwrap(), "failed");
    run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
    assert_eq!(outbox.state_of(1).unwrap(), "delivered");
    assert_eq!(sink.calls(), 4);
}

#[tokio::test]
async fn a_reopened_pending_row_and_an_expired_lease_each_send_once() {
    let (outbox, path) = db();
    let at = now();
    outbox
        .enqueue(&reminder("pending", "telegram", "reminder", false, at), at)
        .unwrap();
    drop(outbox);
    let outbox = Outbox::open(&path).unwrap();
    let sink = Script::ready(vec![DeliveryOutcome::Accepted]);
    run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
    assert_eq!(sink.calls(), 1);
    assert_eq!(outbox.state_of(1).unwrap(), "delivered");

    let (outbox, path) = db();
    outbox
        .enqueue(&reminder("leased", "telegram", "reminder", false, at), at)
        .unwrap();
    outbox.lease(&[1], at - Duration::minutes(3)).unwrap();
    drop(outbox);
    let outbox = Outbox::open(&path).unwrap();
    let sink = Script::ready(vec![DeliveryOutcome::Accepted]);
    run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
    assert_eq!(sink.calls(), 1);
    assert_eq!(outbox.state_of(1).unwrap(), "delivered");
}

#[tokio::test]
async fn a_missing_channel_stays_pending_and_visible() {
    let (outbox, _) = db();
    let at = now();
    outbox
        .enqueue(&reminder("held", "webpush", "", false, at), at)
        .unwrap();
    let sink = Script::held("set LIBERADO_VAPID_PUBLIC_KEY, LIBERADO_VAPID_PRIVATE_KEY, LIBERADO_VAPID_SUBJECT");
    run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
    assert_eq!(outbox.state_of(1).unwrap(), "pending");
    assert_eq!(outbox.attempts_of(1).unwrap(), 0);
    let visible = outbox.list_visible(20).unwrap();
    assert_eq!(visible.len(), 1);
    assert!(visible[0].last_error.as_deref().unwrap().contains("VAPID"));
    let counts = outbox.counts(at).unwrap();
    assert_eq!(counts.pending, 1);
    assert!(counts.oldest_pending_secs.is_some());
}

#[tokio::test]
async fn a_410_removes_the_subscription_and_a_digest_accepts_together() {
    let (outbox, _) = db();
    let at = now();
    let id = outbox
        .upsert_subscription(
            &NewSubscription {
                endpoint: "http://127.0.0.1/fake-subscription".into(),
                p256dh: UA_PUBLIC.into(),
                auth: AUTH.into(),
                label: "  ".into(),
            },
            at,
        )
        .unwrap();
    assert_eq!(outbox.list_subscriptions().unwrap()[0].label, "phone");
    outbox
        .enqueue(&reminder("gone", "webpush", &format!("sub:{id}"), false, at), at)
        .unwrap();
    let sink = Script::ready(vec![DeliveryOutcome::Gone]);
    run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
    assert_eq!(outbox.subscription_count().unwrap(), 0);
    assert_eq!(outbox.state_of(1).unwrap(), "dead");

    for n in 1..=4 {
        outbox
            .enqueue(
                &reminder(&format!("d{n}"), "webpush", "sub:9", true, at),
                at,
            )
            .unwrap();
    }
    let sink = Script::ready(vec![DeliveryOutcome::Accepted]);
    run_pass(&outbox, &sink, at, PassLimits::default()).await.unwrap();
    assert_eq!(sink.calls(), 1);
    for id in 2..=5 {
        assert_eq!(outbox.state_of(id).unwrap(), "delivered");
    }
}

#[test]
fn the_first_cursor_is_now_and_a_duplicate_insert_does_not_block_it() {
    let (outbox, _) = db();
    let at = now();
    assert_eq!(outbox.ensure_cursor("vault", at).unwrap(), at);
    assert_eq!(
        outbox.ensure_cursor("vault", at + Duration::hours(6)).unwrap(),
        at
    );
    let row = reminder("once", "telegram", "reminder", false, at);
    assert!(matches!(
        outbox.enqueue(&row, at).unwrap(),
        EnqueueOutcome::Inserted(_)
    ));
    assert_eq!(outbox.enqueue(&row, at).unwrap(), EnqueueOutcome::Duplicate);
    outbox.record_cursor("vault", at + Duration::minutes(1)).unwrap();
    outbox.record_cursor("vault", at).unwrap();
    assert_eq!(
        outbox.cursor("vault").unwrap(),
        Some(at + Duration::minutes(1))
    );
}

#[test]
fn ack_sets_only_that_row_and_a_wrong_token_does_not() {
    let (outbox, _) = db();
    let at = now();
    outbox
        .enqueue(&reminder("ack", "webpush", "sub:1", false, at), at)
        .unwrap();
    let token = outbox.due_rows(at, 20).unwrap()[0].token.clone();
    outbox.ack(1, "not-the-token", at).unwrap();
    assert!(!outbox.acked(1).unwrap());
    outbox.ack(1, &token, at).unwrap();
    assert!(outbox.acked(1).unwrap());
    outbox.ack(99, &token, at).unwrap();
}

#[test]
fn old_delivered_rows_are_pruned() {
    let (outbox, _) = db();
    let at = now();
    let EnqueueOutcome::Inserted(old) = outbox
        .enqueue(&reminder("old", "telegram", "reminder", false, at), at)
        .unwrap()
    else {
        panic!("insert");
    };
    outbox
        .mark_accepted(&[old], at - Duration::days(31))
        .unwrap();
    assert_eq!(outbox.prune(at).unwrap(), 1);
    let EnqueueOutcome::Inserted(fresh) = outbox
        .enqueue(&reminder("new", "telegram", "reminder", false, at), at)
        .unwrap()
    else {
        panic!("insert");
    };
    outbox.mark_accepted(&[fresh], at).unwrap();
    assert_eq!(outbox.prune(at).unwrap(), 0);
}

#[test]
fn a_clock_jump_includes_the_missed_minute_once() {
    let at = DateTime::parse_from_rfc3339("2026-10-29T14:05:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let cursor = DateTime::parse_from_rfc3339("2026-10-29T13:58:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let missed = DateTime::parse_from_rfc3339("2026-10-29T14:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert!(crate::in_catchup_window(
        missed,
        cursor,
        at,
        Duration::hours(6)
    ));
    assert!(!crate::in_catchup_window(
        missed,
        missed,
        at,
        Duration::hours(6)
    ));
}
