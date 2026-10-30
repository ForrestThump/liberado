use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

use crate::batch::{DeliveryBatch, DeliveryRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramMode {
    Fallback,
    Always,
    Off,
}

impl TelegramMode {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "fallback" => Some(Self::Fallback),
            "always" => Some(Self::Always),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTarget {
    pub channel: &'static str,
    pub target: String,
}

pub struct FanoutInput<'a> {
    pub mode: TelegramMode,
    pub vapid_ok: bool,
    pub subscription_ids: &'a [i64],
    pub telegram_ready: bool,
    pub matrix_room: Option<&'a str>,
}

pub fn plan_targets(input: FanoutInput<'_>) -> (Vec<PlannedTarget>, Option<&'static str>) {
    let webpush = input.vapid_ok && !input.subscription_ids.is_empty();
    let mut rows = Vec::new();
    if webpush {
        push_subscriptions(&mut rows, input.subscription_ids);
    }
    let mut reason = None;
    if telegram_row(input.mode, webpush, input.telegram_ready) {
        if input.mode == TelegramMode::Fallback {
            reason = Some(fallback_reason(input.vapid_ok));
        }
        rows.push(PlannedTarget {
            channel: "telegram",
            target: "reminder".into(),
        });
    }
    if let Some(room) = input.matrix_room.filter(|room| !room.is_empty()) {
        rows.push(PlannedTarget {
            channel: "matrix",
            target: room.to_string(),
        });
    }
    if rows.is_empty() {
        rows.push(PlannedTarget {
            channel: "webpush",
            target: String::new(),
        });
    }
    (rows, reason)
}

fn push_subscriptions(rows: &mut Vec<PlannedTarget>, ids: &[i64]) {
    for id in ids {
        rows.push(PlannedTarget {
            channel: "webpush",
            target: format!("sub:{id}"),
        });
    }
}

fn telegram_row(mode: TelegramMode, webpush: bool, telegram_ready: bool) -> bool {
    if !telegram_ready {
        return false;
    }
    match mode {
        TelegramMode::Off => false,
        TelegramMode::Always => true,
        TelegramMode::Fallback => !webpush,
    }
}

pub fn fallback_reason(vapid_ok: bool) -> &'static str {
    if vapid_ok {
        "no push subscriptions"
    } else {
        "no VAPID keys"
    }
}

#[derive(Debug, Clone)]
pub struct OpenRow {
    pub id: i64,
    pub channel: String,
    pub target: String,
    pub title: String,
    pub body: String,
    pub catch_up: bool,
    pub due_at: DateTime<Utc>,
    pub token: String,
}

pub fn in_catchup_window(
    at: DateTime<Utc>,
    cursor: DateTime<Utc>,
    now: DateTime<Utc>,
    window: Duration,
) -> bool {
    at > cursor && at <= now && now.signed_duration_since(at) <= window
}

pub fn coalesce(rows: Vec<OpenRow>, now: DateTime<Utc>) -> Vec<DeliveryBatch> {
    let mut groups: BTreeMap<(String, String), Vec<OpenRow>> = BTreeMap::new();
    for row in rows {
        groups
            .entry((row.channel.clone(), row.target.clone()))
            .or_default()
            .push(row);
    }
    let mut out = Vec::new();
    for ((channel, target), group) in groups {
        out.extend(batches_for(channel, target, group, now));
    }
    out
}

fn batches_for(
    channel: String,
    target: String,
    group: Vec<OpenRow>,
    now: DateTime<Utc>,
) -> Vec<DeliveryBatch> {
    let (digest, singles) = split_overdue(group, now);
    let mut out = Vec::new();
    if digest.len() > 3 {
        out.push(digest_batch(&channel, &target, digest));
    } else {
        out.extend(digest.into_iter().map(single_batch));
    }
    out.extend(singles.into_iter().map(single_batch));
    out
}

fn split_overdue(group: Vec<OpenRow>, now: DateTime<Utc>) -> (Vec<OpenRow>, Vec<OpenRow>) {
    let mut digest = Vec::new();
    let mut singles = Vec::new();
    for row in group {
        let late = now.signed_duration_since(row.due_at) > Duration::minutes(5);
        if row.catch_up || late {
            digest.push(row);
        } else {
            singles.push(row);
        }
    }
    (digest, singles)
}

fn single_batch(row: OpenRow) -> DeliveryBatch {
    let id = row.id;
    DeliveryBatch {
        channel: row.channel,
        target: row.target,
        title: row.title,
        body: row.body,
        tag: format!("row-{id}"),
        rows: vec![DeliveryRow {
            id,
            token: row.token,
        }],
    }
}

fn digest_batch(channel: &str, target: &str, rows: Vec<OpenRow>) -> DeliveryBatch {
    let count = rows.len();
    let body = rows
        .iter()
        .map(|row| row.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    DeliveryBatch {
        channel: channel.to_string(),
        target: target.to_string(),
        title: format!("{count} reminders"),
        body,
        tag: "digest".into(),
        rows: rows
            .into_iter()
            .map(|row| DeliveryRow {
                id: row.id,
                token: row.token,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        mode: TelegramMode,
        vapid_ok: bool,
        ids: &'a [i64],
        telegram_ready: bool,
    ) -> FanoutInput<'a> {
        FanoutInput {
            mode,
            vapid_ok,
            subscription_ids: ids,
            telegram_ready,
            matrix_room: None,
        }
    }

    #[test]
    fn telegram_mode_parse_accepts_known_values() {
        assert_eq!(
            TelegramMode::parse("fallback"),
            Some(TelegramMode::Fallback)
        );
        assert_eq!(TelegramMode::parse("always"), Some(TelegramMode::Always));
        assert_eq!(TelegramMode::parse("off"), Some(TelegramMode::Off));
        assert_eq!(TelegramMode::parse("dual"), None);
        assert_eq!(TelegramMode::parse(""), None);
    }

    #[test]
    fn fallback_uses_telegram_only_when_web_push_cannot_carry() {
        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, false, &[], true));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel, "telegram");
        assert_eq!(reason, Some("no VAPID keys"));

        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, true, &[], true));
        assert_eq!(rows[0].channel, "telegram");
        assert_eq!(reason, Some("no push subscriptions"));

        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, true, &[7], true));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel, "webpush");
        assert_eq!(rows[0].target, "sub:7");
        assert!(reason.is_none());
    }

    #[test]
    fn always_sends_both_and_off_never_sends_telegram() {
        let (rows, _) = plan_targets(input(TelegramMode::Always, true, &[3], true));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.channel == "telegram"));
        let (rows, _) = plan_targets(input(TelegramMode::Off, true, &[3], true));
        assert!(rows.iter().all(|row| row.channel != "telegram"));
        let (rows, _) = plan_targets(input(TelegramMode::Off, false, &[], false));
        assert_eq!(rows[0].target, "");
    }

    #[test]
    fn the_mode_matrix_covers_vapid_and_subscription_gaps() {
        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, false, &[], false));
        assert_eq!(rows[0].channel, "webpush");
        assert_eq!(rows[0].target, "");
        assert!(reason.is_none());

        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, false, &[], true));
        assert_eq!(rows[0].channel, "telegram");
        assert_eq!(reason, Some("no VAPID keys"));

        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, true, &[], true));
        assert_eq!(rows[0].channel, "telegram");
        assert_eq!(reason, Some("no push subscriptions"));

        let (rows, reason) = plan_targets(input(TelegramMode::Fallback, true, &[2], true));
        assert_eq!(rows[0].channel, "webpush");
        assert_eq!(rows[0].target, "sub:2");
        assert!(reason.is_none());

        let (rows, _) = plan_targets(input(TelegramMode::Always, true, &[2], true));
        assert_eq!(rows.len(), 2);

        let (rows, _) = plan_targets(input(TelegramMode::Off, true, &[2], true));
        assert!(rows.iter().all(|row| row.channel != "telegram"));
    }

    #[test]
    fn a_digest_is_only_for_a_catch_up_burst() {
        let now = DateTime::parse_from_rfc3339("2026-10-29T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let fresh = (1..=4).map(|id| sample(id, false, now)).collect::<Vec<_>>();
        let batches = coalesce(fresh, now);
        assert_eq!(batches.len(), 4);
        assert!(batches.iter().all(|batch| batch.rows.len() == 1));

        let burst = (1..=4)
            .map(|id| sample(id, true, now - Duration::minutes(1)))
            .collect::<Vec<_>>();
        let batches = coalesce(burst, now);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].rows.len(), 4);
        assert_eq!(batches[0].tag, "digest");

        let late = (1..=4)
            .map(|id| sample(id, false, now - Duration::minutes(10)))
            .collect::<Vec<_>>();
        assert_eq!(coalesce(late, now)[0].rows.len(), 4);
    }

    fn sample(id: i64, catch_up: bool, due_at: DateTime<Utc>) -> OpenRow {
        OpenRow {
            id,
            channel: "webpush".into(),
            target: "sub:1".into(),
            title: format!("t{id}"),
            body: format!("b{id}"),
            catch_up,
            due_at,
            token: format!("tok{id}"),
        }
    }

    #[test]
    fn the_window_rejects_a_stamp_older_than_the_policy_and_keeps_one_inside_it() {
        let now = DateTime::parse_from_rfc3339("2026-10-29T15:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let cursor = now - Duration::hours(8);
        let window = Duration::hours(6);
        let old = now - Duration::hours(7);
        let due = now - Duration::minutes(5);
        assert!(!in_catchup_window(old, cursor, now, window));
        assert!(in_catchup_window(due, cursor, now, window));
        assert!(!in_catchup_window(due, now, now, window));
    }
}
