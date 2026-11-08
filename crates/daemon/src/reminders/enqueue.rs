//! Enqueue mechanical reminders and run the outbox worker loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::SecondsFormat;
use chrono::{DateTime, Utc};
use liberado_config::RemindersConfig;
use liberado_outbox::{
    EnqueueOutcome, FanoutInput, NewReminder, Outbox, PassLimits, TelegramMode, fanout_key,
    key_hash_suffix, plan_targets, run_pass,
};
use tracing::info;

use super::delivery::OutboxDelivery;

#[derive(Debug, Clone, PartialEq)]
pub struct MechanicalReminder {
    pub idempotency_key: String,
    pub title: String,
    pub body: String,
    pub due_at: DateTime<Utc>,
    pub catch_up: bool,
    pub kind: String,
    pub source: Option<liberado_outbox::OutboxSource>,
}

pub fn enqueue_mechanical(
    outbox: &Outbox,
    config: &RemindersConfig,
    delivery: &OutboxDelivery,
    telegram_ready: bool,
    items: &[MechanicalReminder],
    now: DateTime<Utc>,
) -> Result<(), String> {
    let vapid_ok = delivery.vapid_configured();
    let sub_ids = outbox.subscription_ids().map_err(|err| err.to_string())?;
    let mode = telegram_mode(&config.channels.telegram.mode);
    let matrix_room = matrix_room(config);
    let (targets, fallback) = plan_targets(FanoutInput {
        mode,
        vapid_ok,
        subscription_ids: &sub_ids,
        telegram_ready,
        matrix_room,
    });
    if let Some(reason) = fallback {
        log_fallback_once(reason);
    }
    for item in items {
        for target in &targets {
            let key = fanout_key(&item.idempotency_key, target.channel, &target.target);
            let row = NewReminder {
                idempotency_key: key,
                channel: target.channel.into(),
                target: target.target.clone(),
                title: item.title.clone(),
                body: item.body.clone(),
                due_at: item.due_at,
                catch_up: item.catch_up,
                kind: Some(item.kind.clone()),
                source: item.source.clone(),
            };
            let outcome = outbox.enqueue(&row, now).map_err(|err| err.to_string())?;
            log_reminder_enqueue(&row, outcome);
        }
    }
    Ok(())
}

/// Ids, kind, due time, and a key hash. The key, path, title, and body stay out.
pub(crate) fn log_reminder_enqueue(row: &NewReminder, outcome: EnqueueOutcome) {
    let kind = row.kind.as_deref().unwrap_or("");
    let due_at = row.due_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let key_hash = key_hash_suffix(&row.idempotency_key);
    let subscription_id = subscription_id(&row.target);
    match outcome {
        EnqueueOutcome::Inserted(row_id) => {
            log_inserted(row_id, kind, &due_at, &key_hash, subscription_id);
        }
        EnqueueOutcome::Duplicate => {
            log_duplicate(kind, &due_at, &key_hash, subscription_id);
        }
    }
}

fn log_inserted(
    row_id: i64,
    kind: &str,
    due_at: &str,
    key_hash: &str,
    subscription_id: Option<i64>,
) {
    // The callsite interest cache is process-wide. A prior test with no subscriber
    // would otherwise stick Interest::never on this line.
    #[cfg(test)]
    let hold = super::enqueue_tests::log_interest::hold();
    match subscription_id {
        Some(subscription_id) => info!(
            row_id,
            kind, subscription_id, due_at, key_hash, "reminder_enqueued"
        ),
        None => info!(row_id, kind, due_at, key_hash, "reminder_enqueued"),
    }
    #[cfg(test)]
    drop(hold);
}

fn log_duplicate(kind: &str, due_at: &str, key_hash: &str, subscription_id: Option<i64>) {
    #[cfg(test)]
    let hold = super::enqueue_tests::log_interest::hold();
    match subscription_id {
        Some(subscription_id) => info!(
            reason = "duplicate",
            key_hash, kind, due_at, subscription_id, "reminder_enqueue_skipped"
        ),
        None => info!(
            reason = "duplicate",
            key_hash, kind, due_at, "reminder_enqueue_skipped"
        ),
    }
    #[cfg(test)]
    drop(hold);
}

fn subscription_id(target: &str) -> Option<i64> {
    let rest = target.strip_prefix("sub:")?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

pub fn spawn_worker(
    outbox: Arc<Outbox>,
    delivery: Arc<OutboxDelivery>,
    config: RemindersConfig,
    display_zone: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let limits = PassLimits {
            max_catchup: config.max_catchup_per_boot,
            max_attempts: i64::from(config.max_attempts),
            display_zone: &display_zone,
        };
        let sink: &dyn liberado_outbox::DeliverySink = delivery.as_ref();
        loop {
            let now = Utc::now();
            if let Err(err) = run_pass(&outbox, sink, now, limits).await {
                tracing::warn!(error = %err, "outbox pass failed");
            }
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    })
}

fn telegram_mode(text: &str) -> TelegramMode {
    TelegramMode::parse(text).unwrap_or(TelegramMode::Fallback)
}

fn matrix_room(config: &RemindersConfig) -> Option<&str> {
    if !config.channels.matrix.enabled {
        return None;
    }
    config.channels.matrix.room.as_deref()
}

static FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);

fn log_fallback_once(reason: &str) {
    if FALLBACK_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    info!(reason, "Telegram carries reminders because Web Push cannot");
}

/// Record the schedule cursor after a mechanical enqueue commits.
pub fn record_schedule_fire(
    outbox: &Outbox,
    schedule: &str,
    fire: DateTime<Utc>,
) -> Result<(), String> {
    outbox
        .record_cursor(schedule, fire)
        .map_err(|err| err.to_string())
}

/// First boot: anchor the cursor at `now` without replay.
pub fn ensure_schedule_cursor(
    outbox: &Outbox,
    schedule: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    outbox
        .ensure_cursor(schedule, now)
        .map_err(|err| err.to_string())
}
