//! Application-service listener. Every request checks the homeserver token.
//!
//! Routes are the Matrix application-service paths plus the same paths without the
//! `/_matrix/app/v1` prefix. A repeated transaction id returns `{}` and is not processed again.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use liberado_markdown::markdown_to_html;
use liberado_messaging::InboundEvent;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::client::MatrixClient;
use crate::filter::{AcceptPolicy, Class, ENCRYPTED_ROOM_NOTICE, classify, parse_event};
use crate::hub::RoomQueues;
use crate::messages::{MsgKind, message_content};
use crate::token::{AuthError, check_token};
use crate::txn_log::TxnLog;

pub struct ListenerState {
    hs_token: String,
    txns: Arc<TxnLog>,
    queues: Arc<RoomQueues>,
    policy: AcceptPolicy,
    client: MatrixClient,
    notified: Mutex<HashSet<String>>,
    main_user: String,
    known_rooms: Vec<String>,
}

impl ListenerState {
    pub(crate) fn new(
        hs_token: String,
        txns: Arc<TxnLog>,
        queues: Arc<RoomQueues>,
        policy: AcceptPolicy,
        client: MatrixClient,
        main_user: String,
        known_rooms: Vec<String>,
    ) -> Self {
        Self {
            hs_token,
            txns,
            queues,
            policy,
            client,
            notified: Mutex::new(HashSet::new()),
            main_user,
            known_rooms,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct AccessQuery {
    #[serde(default)]
    access_token: Option<String>,
}

pub(crate) fn router(state: Arc<ListenerState>) -> Router {
    Router::new()
        .route(
            "/_matrix/app/v1/transactions/{txn_id}",
            put(put_transaction),
        )
        .route("/transactions/{txn_id}", put(put_transaction))
        .route("/_matrix/app/v1/users/{user_id}", get(get_user))
        .route("/users/{user_id}", get(get_user))
        .route("/_matrix/app/v1/rooms/{alias}", get(get_room))
        .route("/rooms/{alias}", get(get_room))
        .route("/_matrix/app/v1/ping", post(post_ping))
        .route("/ping", post(post_ping))
        .with_state(state)
}

/// Bind `addr` and serve on a background task. A bind failure is returned to the caller.
///
/// The returned address is the one the socket actually bound. Port `0` becomes that port.
pub async fn bind_and_serve(
    addr: SocketAddr,
    state: Arc<ListenerState>,
) -> Result<(SocketAddr, JoinHandle<()>), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|error| format!("matrix listen {addr}: {error}"))?;
    let bound = listener.local_addr().unwrap_or(addr);
    let app = router(state);
    Ok((bound, tokio::spawn(serve(listener, app))))
}

async fn serve(listener: TcpListener, app: Router) {
    if let Err(error) = axum::serve(listener, app).await {
        tracing::warn!(%error, "matrix listener stopped");
    }
}

async fn put_transaction(
    State(state): State<Arc<ListenerState>>,
    Path(txn_id): Path<String>,
    Query(query): Query<AccessQuery>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if let Some(response) = reject(&state, &headers, &query) {
        return response;
    }
    let events = match parse_txn(&body) {
        Ok(events) => events,
        Err(()) => return bad_json(),
    };
    accept_txn(&state, &txn_id, &events).await
}

async fn get_user(
    State(state): State<Arc<ListenerState>>,
    Path(user_id): Path<String>,
    Query(query): Query<AccessQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = reject(&state, &headers, &query) {
        return response;
    }
    if user_id.eq_ignore_ascii_case(&state.main_user) {
        empty_ok()
    } else {
        not_found()
    }
}

async fn get_room(
    State(state): State<Arc<ListenerState>>,
    Path(alias): Path<String>,
    Query(query): Query<AccessQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = reject(&state, &headers, &query) {
        return response;
    }
    if state
        .known_rooms
        .iter()
        .any(|room| room.eq_ignore_ascii_case(&alias))
    {
        empty_ok()
    } else {
        not_found()
    }
}

async fn post_ping(
    State(state): State<Arc<ListenerState>>,
    Query(query): Query<AccessQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = reject(&state, &headers, &query) {
        return response;
    }
    empty_ok()
}

fn reject(state: &ListenerState, headers: &HeaderMap, query: &AccessQuery) -> Option<Response> {
    let header = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    match check_token(&state.hs_token, header, query.access_token.as_deref()) {
        Ok(()) => None,
        Err(AuthError::Unknown) => Some(errcode(StatusCode::UNAUTHORIZED, "M_UNKNOWN_TOKEN")),
        Err(AuthError::Forbidden) => Some(errcode(StatusCode::FORBIDDEN, "M_FORBIDDEN")),
    }
}

fn parse_txn(body: &str) -> Result<Vec<Value>, ()> {
    if body.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(body).map_err(|_| ())?;
    Ok(value
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn accept_txn(state: &ListenerState, txn_id: &str, events: &[Value]) -> Response {
    match state.txns.record(txn_id).await {
        Ok(true) => {}
        Ok(false) => return empty_ok(),
        Err(error) => {
            tracing::warn!(%error, "matrix: could not persist txn id");
            return errcode(StatusCode::INTERNAL_SERVER_ERROR, "M_UNKNOWN");
        }
    }
    dispatch_events(state, events).await;
    empty_ok()
}

async fn dispatch_events(state: &ListenerState, events: &[Value]) {
    for event in events {
        let Some(parsed) = parse_event(event) else {
            continue;
        };
        dispatch_one(state, &parsed).await;
    }
}

async fn dispatch_one(state: &ListenerState, event: &crate::filter::RawEvent) {
    match classify(&event.sender, &event.event_type, &state.policy) {
        Class::Echo => tracing::debug!(room = %event.room_id, "matrix: ignored own echo"),
        Class::Stranger => {
            tracing::debug!(room = %event.room_id, "matrix: ignored non-owner sender")
        }
        Class::Other => {}
        Class::Encrypted => notify_encrypted(state, &event.room_id).await,
        Class::Message => push_message(state, event),
    }
}

fn push_message(state: &ListenerState, event: &crate::filter::RawEvent) {
    if event.body.trim().is_empty() {
        return;
    }
    state.queues.push(
        &event.room_id,
        InboundEvent::Message {
            text: event.body.clone(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
}

async fn notify_encrypted(state: &ListenerState, room_id: &str) {
    if !claim_notice(state, room_id) {
        return;
    }
    let html = markdown_to_html(ENCRYPTED_ROOM_NOTICE);
    let content = message_content(MsgKind::Notice, ENCRYPTED_ROOM_NOTICE, &html);
    if let Err(error) = state.client.send_message(room_id, &content).await {
        tracing::warn!(%error, room = room_id, "matrix encrypted-room notice failed");
    }
}

fn claim_notice(state: &ListenerState, room_id: &str) -> bool {
    let Ok(mut seen) = state.notified.lock() else {
        return false;
    };
    seen.insert(room_id.to_string())
}

fn errcode(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "errcode": code, "error": code }))).into_response()
}

fn empty_ok() -> Response {
    Json(json!({})).into_response()
}

fn not_found() -> Response {
    errcode(StatusCode::NOT_FOUND, "M_NOT_FOUND")
}

fn bad_json() -> Response {
    errcode(StatusCode::BAD_REQUEST, "M_NOT_JSON")
}

#[cfg(test)]
#[path = "listener_tests.rs"]
mod tests;
