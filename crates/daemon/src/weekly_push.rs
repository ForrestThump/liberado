//! One weekly spending push per subscription.
//!
//! Subscriptions live in the outbox database. A successful send stores the Monday
//! date on that row. A failed send leaves the date alone, so the next run tries
//! again. 404 and 410 delete the subscription.

use std::sync::Arc;

use async_trait::async_trait;
use liberado_notify::web_push::{self, PushDelivery, VapidKeys};
use liberado_outbox::{Outbox, WeekTarget};

const WEEKLY_TTL: u64 = 24 * 60 * 60;

/// Counts from one fan-out. No endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WeekReport {
    pub sent: usize,
    pub failed: usize,
    pub removed: usize,
    pub skipped: usize,
    pub unconfigured: bool,
}

/// The weekly job's send port. Tests substitute a quiet sender.
#[async_trait]
pub trait WeekSender: Send + Sync {
    fn needs_week(&self, week_id: &str) -> bool;
    async fn send_week(&self, week_id: &str, title: &str, body: &str, force: bool) -> WeekReport;
}

/// Outbox subscriptions plus the process VAPID keys.
pub struct WeekPush {
    outbox: Arc<Outbox>,
    keys: Option<VapidKeys>,
}

impl WeekPush {
    pub fn new(outbox: Arc<Outbox>, keys: Option<VapidKeys>) -> Self {
        Self { outbox, keys }
    }
}

/// No subscriptions and no keys. A dry run does not call it.
pub fn inert_week_sender() -> Arc<dyn WeekSender> {
    Arc::new(InertWeek)
}

struct InertWeek;

#[async_trait]
impl WeekSender for InertWeek {
    fn needs_week(&self, _week_id: &str) -> bool {
        false
    }

    async fn send_week(
        &self,
        _week_id: &str,
        _title: &str,
        _body: &str,
        _force: bool,
    ) -> WeekReport {
        WeekReport::default()
    }
}

#[async_trait]
impl WeekSender for WeekPush {
    fn needs_week(&self, week_id: &str) -> bool {
        self.outbox.needs_week(week_id).unwrap_or(true)
    }

    async fn send_week(&self, week_id: &str, title: &str, body: &str, force: bool) -> WeekReport {
        let Some(keys) = self.keys.as_ref() else {
            tracing::info!("weekly spending push is off; VAPID keys are unset");
            return WeekReport {
                unconfigured: true,
                ..WeekReport::default()
            };
        };
        let targets = match self.outbox.week_targets() {
            Ok(rows) => rows,
            Err(err) => {
                tracing::warn!(error = %err, "weekly spending could not read subscriptions");
                return WeekReport {
                    failed: 1,
                    ..WeekReport::default()
                };
            }
        };
        fan_out(&self.outbox, keys, &targets, week_id, title, body, force).await
    }
}

async fn fan_out(
    outbox: &Outbox,
    keys: &VapidKeys,
    targets: &[WeekTarget],
    week_id: &str,
    title: &str,
    body: &str,
    force: bool,
) -> WeekReport {
    let topic = topic_for(week_id);
    let payload = weekly_payload(title, body, &topic);
    let mut report = WeekReport {
        skipped: skipped(targets, week_id, force),
        ..WeekReport::default()
    };
    for target in targets {
        if due(target, week_id, force) {
            one(&mut report, outbox, keys, target, &payload, &topic, week_id).await;
        }
    }
    report
}

async fn one(
    report: &mut WeekReport,
    outbox: &Outbox,
    keys: &VapidKeys,
    target: &WeekTarget,
    payload: &str,
    topic: &str,
    week_id: &str,
) {
    let delivery = web_push::post_push(
        keys,
        web_push::PushRequest {
            endpoint: &target.endpoint,
            p256dh: &target.p256dh,
            auth: &target.auth,
            payload: payload.as_bytes(),
            ttl_secs: WEEKLY_TTL,
            urgency: "high",
            topic: Some(topic),
        },
        web_push::PushAttemptIds {
            row_ids: &[],
            subscription_id: target.id,
        },
    )
    .await;
    match delivery {
        PushDelivery::Accepted => record_sent(report, outbox, target.id, week_id),
        PushDelivery::Gone => record_gone(report, outbox, target.id),
        PushDelivery::Failed => report.failed += 1,
    }
}

fn record_sent(report: &mut WeekReport, outbox: &Outbox, id: i64, week_id: &str) {
    report.sent += 1;
    if let Err(err) = outbox.mark_week_sent(id, week_id) {
        tracing::warn!(error = %err, "weekly spending sent but did not store the week");
    }
}

fn record_gone(report: &mut WeekReport, outbox: &Outbox, id: i64) {
    report.removed += 1;
    if let Err(err) = outbox.delete_subscription(id) {
        tracing::warn!(error = %err, "weekly spending could not drop a gone subscription");
    }
}

fn skipped(targets: &[WeekTarget], week_id: &str, force: bool) -> usize {
    if force {
        return 0;
    }
    targets
        .iter()
        .filter(|target| already_sent(target, week_id))
        .count()
}

fn due(target: &WeekTarget, week_id: &str, force: bool) -> bool {
    force || !already_sent(target, week_id)
}

fn already_sent(target: &WeekTarget, week_id: &str) -> bool {
    target.last_sent_week.as_deref() == Some(week_id)
}

fn topic_for(week_id: &str) -> String {
    let mut topic = String::from("spend");
    for ch in week_id.chars().filter(|ch| ch.is_ascii_alphanumeric()) {
        if topic.len() == 32 {
            break;
        }
        topic.push(ch);
    }
    topic
}

fn weekly_payload(title: &str, body: &str, tag: &str) -> String {
    serde_json::json!({
        "title": title,
        "body": body,
        "tag": tag,
        "url": "/",
    })
    .to_string()
}
