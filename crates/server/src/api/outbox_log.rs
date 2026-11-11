//! Structured logs for outbox actions. Ids and timestamps only.

use chrono::{DateTime, SecondsFormat, Utc};
use tracing::{info, warn};

pub(crate) const VIA_DETAIL: &str = "detail";
pub(crate) const VIA_TRAY: &str = "tray";

macro_rules! action_event {
    ($result:expr, $($fields:tt)*) => {
        if $result == "error" {
            tracing::warn!($($fields)*);
        } else {
            tracing::info!($($fields)*);
        }
    };
}

pub(crate) fn log_outbox_action(
    action: &str,
    via: &str,
    row_id: i64,
    received_at: DateTime<Utc>,
    result: &str,
) {
    let received_at = rfc3339(received_at);
    action_event!(
        result,
        action,
        via,
        row_id,
        received_at = %received_at,
        result,
        "outbox_action"
    );
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
}

pub(crate) fn log_snooze_stopped(
    via: &str,
    row_id: i64,
    minutes: i64,
    received_at: DateTime<Utc>,
    result: &str,
) {
    let received_at = rfc3339(received_at);
    action_event!(
        result,
        action = "snooze",
        via,
        row_id,
        minutes,
        received_at = %received_at,
        result,
        "outbox_action"
    );
}

pub(crate) fn log_body_tap_ack(row_id: i64, received_at: DateTime<Utc>) {
    let received_at = rfc3339(received_at);
    info!(
        row_id,
        received_at = %received_at,
        ack = "body_tap",
        "outbox_ack"
    );
}

pub(crate) fn log_ack_failed(row_id: i64) {
    warn!(row_id, "outbox_ack failed");
}

fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}
