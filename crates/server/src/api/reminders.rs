//! Web Push subscriptions, outbox visibility, and unauthenticated click acks.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use liberado_common::UserTimezone;
use liberado_outbox::{NewSubscription, OutboxSource, TEST, copy};
use serde::Deserialize;
use tracing::{info, warn};

use crate::state::AppState;

#[derive(Deserialize)]
pub struct SubscribeBody {
    endpoint: String,
    keys: SubscribeKeys,
    #[serde(default)]
    label: String,
}

#[derive(Deserialize)]
struct SubscribeKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
pub struct EndpointBody {
    endpoint: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct TestBody {
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    id: Option<i64>,
}

#[derive(Deserialize)]
pub struct AckBody {
    id: i64,
    token: String,
}

pub async fn list_subscriptions(State(state): State<Arc<AppState>>) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    match outbox.list_subscriptions() {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn push_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let count = state
        .outbox
        .as_ref()
        .and_then(|outbox| outbox.subscription_count().ok())
        .unwrap_or(0);
    Json(serde_json::json!({
        "vapid_configured": state.outbox_delivery.as_ref().is_some_and(|d| d.vapid_configured()),
        "subscription_count": count,
        "public_key": state.outbox_delivery.as_ref().and_then(|d| d.vapid_public_key()),
    }))
}

pub async fn vapid_public_key(State(state): State<Arc<AppState>>) -> Response {
    match state
        .outbox_delivery
        .as_ref()
        .and_then(|d| d.vapid_public_key())
    {
        Some(public_key) => Json(serde_json::json!({ "public_key": public_key })).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "Web Push keys are not configured" })),
        )
            .into_response(),
    }
}

pub async fn upsert_subscription(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SubscribeBody>,
) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let push_host = endpoint_host(&body.endpoint);
    match outbox.upsert_subscription(
        &NewSubscription {
            endpoint: body.endpoint,
            p256dh: body.keys.p256dh,
            auth: body.keys.auth,
            label: body.label,
        },
        Utc::now(),
    ) {
        Ok(id) => {
            let count = outbox.subscription_count().unwrap_or(0);
            info!(
                push_host = %push_host,
                subscription_id = id,
                subscription_count = count,
                "push subscription upserted"
            );
            Json(serde_json::json!({ "ok": true, "id": id })).into_response()
        }
        Err(err) => {
            warn!(
                push_host = %push_host,
                error = %err,
                "push subscription upsert failed"
            );
            fail(StatusCode::BAD_REQUEST, &err.to_string())
        }
    }
}

pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EndpointBody>,
) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let push_host = endpoint_host(&body.endpoint);
    match outbox.delete_subscription_endpoint(&body.endpoint) {
        Ok(true) => {
            let count = outbox.subscription_count().unwrap_or(0);
            info!(
                push_host = %push_host,
                subscription_count = count,
                "push subscription removed"
            );
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Ok(false) => {
            warn!(push_host = %push_host, "push subscription delete not found");
            fail(StatusCode::NOT_FOUND, "subscription not found")
        }
        Err(err) => {
            warn!(
                push_host = %push_host,
                error = %err,
                "push subscription delete failed"
            );
            fail(StatusCode::BAD_REQUEST, &err.to_string())
        }
    }
}

pub async fn push_test(State(state): State<Arc<AppState>>, Json(body): Json<TestBody>) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let subs = match outbox.list_subscriptions() {
        Ok(rows) => rows,
        Err(err) => return fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    };
    if subs.is_empty() {
        return fail(
            StatusCode::CONFLICT,
            "no subscribed device — enable push on a device first",
        );
    }
    let matched = match (body.id, body.endpoint.as_deref()) {
        (Some(id), _) => subs.iter().find(|sub| sub.id == id),
        (None, Some(endpoint)) => subs.iter().find(|sub| sub.endpoint == endpoint),
        (None, None) => None,
    };
    let Some(sub) = matched else {
        return fail(
            StatusCode::NOT_FOUND,
            "this device is not subscribed — turn push on for this browser",
        );
    };
    let now = Utc::now();
    let zone = state
        .config
        .topology
        .user_timezone()
        .ok()
        .map(|tz| tz.iana_name().to_string())
        .unwrap_or_else(|| UserTimezone::default_zone().iana_name().to_string());
    let target = format!("sub:{}", sub.id);
    let key = format!("test/{}", now.timestamp());
    if let Err(err) = outbox.enqueue(
        &liberado_outbox::NewReminder {
            idempotency_key: key,
            channel: "webpush".into(),
            target,
            title: "Liberado test".into(),
            body: copy::test_notification_body(&zone, now),
            due_at: now,
            catch_up: false,
            kind: Some(TEST.into()),
            source: Some(OutboxSource::Settings),
        },
        now,
    ) {
        warn!(error = %err, "push test enqueue failed");
        return fail(StatusCode::BAD_REQUEST, &err.to_string());
    }
    info!(
        subscription_id = sub.id,
        push_host = %sub.host,
        "push test notification enqueued"
    );
    Json(serde_json::json!({ "ok": true })).into_response()
}

pub async fn list_outbox(State(state): State<Arc<AppState>>) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    match outbox.list_visible(100) {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => fail(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub async fn retry_outbox(State(state): State<Arc<AppState>>, Path(id): Path<i64>) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    match outbox.retry(id, Utc::now()) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => fail(StatusCode::NOT_FOUND, "row not found or not retriable"),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

pub async fn outbox_ack_with_state(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AckBody>,
) -> Response {
    if let Some(outbox) = state.outbox.as_ref() {
        let _ = outbox.ack(body.id, &body.token, Utc::now());
    }
    StatusCode::NO_CONTENT.into_response()
}

fn fail(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

#[derive(Deserialize)]
pub struct ClientErrorBody {
    message: String,
}

pub(crate) fn truncate_client_error_message(message: &str) -> String {
    message.chars().take(500).collect()
}

pub async fn report_client_error(Json(body): Json<ClientErrorBody>) -> impl IntoResponse {
    let truncated = truncate_client_error_message(&body.message);
    warn!(client_error = %truncated, "webui client error");
    StatusCode::NO_CONTENT
}

fn endpoint_host(endpoint: &str) -> String {
    endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("unknown")
        .to_string()
}

#[cfg(test)]
mod tests {
    use liberado_outbox::VisibleReminder;

    #[test]
    fn visible_row_shape_matches_contract() {
        let row = VisibleReminder {
            id: 1,
            channel: "webpush".into(),
            target_host: "push.example".into(),
            title: "t".into(),
            state: "pending".into(),
            attempts: 0,
            last_error: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&row).unwrap();
        assert!(json.get("id").is_some());
    }

    #[test]
    fn client_error_message_truncates_at_five_hundred_chars() {
        let long = "x".repeat(600);
        let out = super::truncate_client_error_message(&long);
        assert_eq!(out.len(), 500);
        assert_eq!(out.chars().count(), 500);
    }
}
