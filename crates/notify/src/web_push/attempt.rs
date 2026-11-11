//! The `web_push_attempt` record.
//!
//! The delivery worker builds [`PushAttemptIds`]. `post_push` records the
//! provider outcome. The endpoint, payload, and transport error text stay out.

use chrono::{DateTime, SecondsFormat, Utc};
use tracing::Level;

use super::send::{PushDelivery, origin_of};

/// Ids for the worker log. The endpoint and payload are not logged.
pub struct PushAttemptIds<'a> {
    pub row_ids: &'a [i64],
    pub subscription_id: i64,
}

pub(super) struct Classified {
    pub(super) delivery: PushDelivery,
    pub(super) outcome: &'static str,
    pub(super) warn: bool,
}

pub(super) fn classify(status: reqwest::StatusCode) -> Classified {
    let code = status.as_u16();
    if (200..300).contains(&code) {
        return Classified {
            delivery: PushDelivery::Accepted,
            outcome: "provider_accepted",
            warn: false,
        };
    }
    if code == 404 || code == 410 {
        return Classified {
            delivery: PushDelivery::Gone,
            outcome: "gone",
            warn: false,
        };
    }
    Classified {
        delivery: PushDelivery::Failed,
        outcome: "rejected",
        warn: true,
    }
}

macro_rules! push_attempt {
    ($warn:expr, $($fields:tt)*) => {
        if $warn {
            tracing::warn!($($fields)*);
        } else {
            tracing::info!($($fields)*);
        }
    };
}

pub(super) fn log_push_http(
    endpoint: &str,
    attempt: &PushAttemptIds<'_>,
    attempt_at: DateTime<Utc>,
    class: &Classified,
    status: u16,
) {
    let host = host_of(endpoint);
    let row_ids = join_row_ids(attempt.row_ids);
    let attempt_at = attempt_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    // The callsite level is const. One macro keeps the field list in one place.
    push_attempt!(
        class.warn,
        row_ids = %row_ids,
        subscription_id = attempt.subscription_id,
        push_host = %host,
        outcome = class.outcome,
        status,
        attempt_at = %attempt_at,
        "web_push_attempt"
    );
}

pub(super) fn log_push_transport(
    endpoint: &str,
    attempt: &PushAttemptIds<'_>,
    attempt_at: DateTime<Utc>,
) {
    let host = host_of(endpoint);
    let row_ids = join_row_ids(attempt.row_ids);
    let attempt_at = attempt_at.to_rfc3339_opts(SecondsFormat::Secs, true);
    tracing::event!(
        Level::WARN,
        row_ids = %row_ids,
        subscription_id = attempt.subscription_id,
        push_host = %host,
        outcome = "transport_error",
        attempt_at = %attempt_at,
        "web_push_attempt"
    );
}

/// Host, including port, with the path removed. A missing origin is `push`.
pub(super) fn host_of(endpoint: &str) -> String {
    origin_of(endpoint)
        .ok()
        .and_then(|origin| origin.split("://").nth(1).map(str::to_string))
        .unwrap_or_else(|| "push".into())
}

fn join_row_ids(ids: &[i64]) -> String {
    let mut out = String::new();
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&id.to_string());
    }
    out
}
