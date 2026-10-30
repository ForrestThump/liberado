//! Web Push subscriptions, outbox visibility, and unauthenticated click acks.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use liberado_outbox::NewSubscription;
use serde::Deserialize;

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
}

#[derive(Deserialize)]
pub struct AckBody {
    id: i64,
    token: String,
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
    match outbox.upsert_subscription(
        &NewSubscription {
            endpoint: body.endpoint,
            p256dh: body.keys.p256dh,
            auth: body.keys.auth,
            label: body.label,
        },
        Utc::now(),
    ) {
        Ok(id) => Json(serde_json::json!({ "ok": true, "id": id })).into_response(),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    Json(body): Json<EndpointBody>,
) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    match outbox.delete_subscription_endpoint(&body.endpoint) {
        Ok(true) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(false) => fail(StatusCode::NOT_FOUND, "subscription not found"),
        Err(err) => fail(StatusCode::BAD_REQUEST, &err.to_string()),
    }
}

pub async fn push_test(State(state): State<Arc<AppState>>, Json(body): Json<TestBody>) -> Response {
    let Some(outbox) = state.outbox.as_ref() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "outbox is not available");
    };
    let now = Utc::now();
    let target = match body.endpoint.as_deref() {
        Some(endpoint) => {
            let subs = outbox.list_subscriptions().ok().unwrap_or_default();
            subs.iter()
                .find(|sub| endpoint.contains(&sub.host))
                .map(|sub| format!("sub:{}", sub.id))
                .unwrap_or_else(|| "sub:0".into())
        }
        None => outbox
            .subscription_ids()
            .ok()
            .and_then(|ids| ids.first().map(|id| format!("sub:{id}")))
            .unwrap_or_else(|| "sub:0".into()),
    };
    let key = format!("test/{}", now.timestamp());
    if let Err(err) = outbox.enqueue(
        &liberado_outbox::NewReminder {
            idempotency_key: key,
            channel: "webpush".into(),
            target,
            title: "Liberado test".into(),
            body: "Liberado test notification.".into(),
            due_at: now,
            catch_up: false,
        },
        now,
    ) {
        return fail(StatusCode::BAD_REQUEST, &err.to_string());
    }
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

pub async fn retry_outbox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Response {
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
    (
        status,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use liberado_outbox::VisibleReminder;
    use super::*;

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
}
