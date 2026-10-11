use std::sync::Arc;
use std::time::Duration;

use liberado_messaging::InboundEvent;
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use crate::client::MatrixClient;
use crate::encode::encode_segment;
use crate::filter::AcceptPolicy;
use crate::filter::ENCRYPTED_ROOM_NOTICE;
use crate::hub::RoomQueues;
use crate::listener::ListenerState;
use crate::txn_log::TxnLog;

const HS: &str = "hs-secret";
const ROOM: &str = "!room:matrix.homelab.local";
const OWNER: &str = "@shiloh:matrix.homelab.local";
const MAIN: &str = "@liberado:matrix.homelab.local";

struct Harness {
    base: String,
    incoming: UnboundedReceiver<InboundEvent>,
    _server: tokio::task::JoinHandle<()>,
}

fn policy() -> AcceptPolicy {
    AcceptPolicy {
        owner: OWNER.into(),
        server_name: "matrix.homelab.local".into(),
        main_user: MAIN.into(),
        sender_localpart: "liberado_bridge".into(),
    }
}

fn state(txns: Arc<TxnLog>, queues: Arc<RoomQueues>, client: MatrixClient) -> Arc<ListenerState> {
    Arc::new(ListenerState::new(
        HS.into(),
        txns,
        queues,
        policy(),
        client,
        MAIN.into(),
        vec![ROOM.into(), "#liberado:matrix.homelab.local".into()],
    ))
}

async fn harness(dir: &std::path::Path, homeserver: &str) -> Harness {
    let queues = Arc::new(RoomQueues::new());
    let incoming = queues.register(ROOM);
    let txns = Arc::new(TxnLog::load(dir.join("matrix-txns.json")));
    let client = MatrixClient::new(homeserver, "as-token", MAIN).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = super::router(state(txns, queues, client));
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Harness {
        base: format!("http://{addr}"),
        incoming,
        _server: server,
    }
}

async fn exchange(
    base: &str,
    verb: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let client = reqwest::Client::new();
    let url = format!("{base}{path}");
    let mut last = String::new();
    for _ in 0..20 {
        let mut request = client.request(verb_of(verb), &url);
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                return (status, text);
            }
            Err(error) => {
                last = error.to_string();
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
    panic!("listener request failed: {last}");
}

fn verb_of(verb: &str) -> reqwest::Method {
    verb.parse().unwrap()
}

fn event(sender: &str, event_type: &str, body: &str) -> String {
    json!({
        "events": [{
            "type": event_type,
            "room_id": ROOM,
            "sender": sender,
            "content": { "body": body, "msgtype": "m.text" }
        }]
    })
    .to_string()
}

fn next_text(incoming: &mut UnboundedReceiver<InboundEvent>) -> Option<String> {
    match incoming.try_recv().ok()? {
        InboundEvent::Message { text, .. } => Some(text),
        other => panic!("expected a message, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_and_wrong_tokens_are_rejected_on_each_route() {
    let dir = tempfile::tempdir().unwrap();
    let homeserver = MockServer::start().await;
    let live = harness(dir.path(), &homeserver.uri()).await;
    let routes = [
        ("PUT", "/_matrix/app/v1/transactions/auth-a".to_string()),
        ("PUT", "/transactions/auth-b".to_string()),
        (
            "GET",
            format!("/_matrix/app/v1/users/{}", encode_segment(MAIN)),
        ),
        ("GET", format!("/users/{}", encode_segment(MAIN))),
        (
            "GET",
            format!("/_matrix/app/v1/rooms/{}", encode_segment(ROOM)),
        ),
        ("GET", format!("/rooms/{}", encode_segment(ROOM))),
        ("POST", "/_matrix/app/v1/ping".to_string()),
        ("POST", "/ping".to_string()),
    ];
    for (verb, path) in &routes {
        let (status, body) = exchange(&live.base, verb, path, None, None).await;
        assert_eq!(status, 401, "{verb} {path} {body}");
        assert!(body.contains("M_UNKNOWN_TOKEN"), "{body}");
        let (status, body) = exchange(&live.base, verb, path, Some("nope"), None).await;
        assert_eq!(status, 403, "{verb} {path} {body}");
        assert!(body.contains("M_FORBIDDEN"), "{body}");
    }
}

#[tokio::test]
async fn a_wrong_bearer_does_not_use_the_query_token() {
    let dir = tempfile::tempdir().unwrap();
    let homeserver = MockServer::start().await;
    let live = harness(dir.path(), &homeserver.uri()).await;
    let path = format!("/ping?access_token={HS}");
    let (status, body) = exchange(&live.base, "POST", &path, Some("nope"), None).await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = exchange(&live.base, "POST", &path, None, None).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn known_users_and_rooms_answer_and_the_rest_are_missing() {
    let dir = tempfile::tempdir().unwrap();
    let homeserver = MockServer::start().await;
    let live = harness(dir.path(), &homeserver.uri()).await;
    let user = format!("/_matrix/app/v1/users/{}", encode_segment(MAIN));
    let (status, _) = exchange(&live.base, "GET", &user, Some(HS), None).await;
    assert_eq!(status, 200);
    let missing = format!("/users/{}", encode_segment("@other:matrix.homelab.local"));
    let (status, body) = exchange(&live.base, "GET", &missing, Some(HS), None).await;
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("M_NOT_FOUND"));
    let room = format!(
        "/rooms/{}",
        encode_segment("#liberado:matrix.homelab.local")
    );
    let (status, _) = exchange(&live.base, "GET", &room, Some(HS), None).await;
    assert_eq!(status, 200);
    let absent = format!(
        "/_matrix/app/v1/rooms/{}",
        encode_segment("#nope:matrix.homelab.local")
    );
    let (status, _) = exchange(&live.base, "GET", &absent, Some(HS), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn the_owner_message_is_queued_and_other_senders_are_not() {
    let dir = tempfile::tempdir().unwrap();
    let homeserver = MockServer::start().await;
    let mut live = harness(dir.path(), &homeserver.uri()).await;
    let owner = event(OWNER, "m.room.message", "status");
    let (status, body) = exchange(
        &live.base,
        "PUT",
        "/_matrix/app/v1/transactions/owner-1",
        Some(HS),
        Some(&owner),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(next_text(&mut live.incoming).as_deref(), Some("status"));

    let echo = event(MAIN, "m.room.message", "echo");
    let (status, _) = exchange(
        &live.base,
        "PUT",
        "/transactions/echo-1",
        Some(HS),
        Some(&echo),
    )
    .await;
    assert_eq!(status, 200);
    let stranger = event("@other:matrix.homelab.local", "m.room.message", "nope");
    let (status, _) = exchange(
        &live.base,
        "PUT",
        "/_matrix/app/v1/transactions/stranger-1",
        Some(HS),
        Some(&stranger),
    )
    .await;
    assert_eq!(status, 200);
    assert!(next_text(&mut live.incoming).is_none());
}

#[tokio::test]
async fn a_repeated_transaction_is_not_processed_again_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let homeserver = MockServer::start().await;
    let mut live = harness(dir.path(), &homeserver.uri()).await;
    let first = event(OWNER, "m.room.message", "one");
    let path = "/_matrix/app/v1/transactions/same-txn";
    let (status, _) = exchange(&live.base, "PUT", path, Some(HS), Some(&first)).await;
    assert_eq!(status, 200);
    assert_eq!(next_text(&mut live.incoming).as_deref(), Some("one"));
    let second = event(OWNER, "m.room.message", "two");
    let (status, _) = exchange(&live.base, "PUT", path, Some(HS), Some(&second)).await;
    assert_eq!(status, 200);
    assert!(next_text(&mut live.incoming).is_none());

    live._server.abort();
    let mut restarted = harness(dir.path(), &homeserver.uri()).await;
    let (status, _) = exchange(&restarted.base, "PUT", path, Some(HS), Some(&second)).await;
    assert_eq!(status, 200);
    assert!(next_text(&mut restarted.incoming).is_none());
}

#[tokio::test]
async fn an_encrypted_event_sends_one_notice() {
    let dir = tempfile::tempdir().unwrap();
    let homeserver = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$notice"})))
        .mount(&homeserver)
        .await;
    let mut live = harness(dir.path(), &homeserver.uri()).await;
    let encrypted = event(OWNER, "m.room.encrypted", "");
    for txn in ["enc-1", "enc-2"] {
        let path = format!("/_matrix/app/v1/transactions/{txn}");
        let (status, body) = exchange(&live.base, "PUT", &path, Some(HS), Some(&encrypted)).await;
        assert_eq!(status, 200, "{body}");
    }
    assert!(next_text(&mut live.incoming).is_none());
    let requests = homeserver.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["msgtype"], "m.notice");
    assert_eq!(body["body"], ENCRYPTED_ROOM_NOTICE);
}

#[tokio::test]
async fn a_persist_failure_returns_500_and_does_not_queue_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("matrix-txns.json");
    std::fs::create_dir(&blocked).unwrap();
    let homeserver = MockServer::start().await;
    let mut live = harness(dir.path(), &homeserver.uri()).await;
    let message = event(OWNER, "m.room.message", "kept");
    let (status, body) = exchange(
        &live.base,
        "PUT",
        "/_matrix/app/v1/transactions/fail-1",
        Some(HS),
        Some(&message),
    )
    .await;
    assert_eq!(status, 500, "{body}");
    assert!(body.contains("M_UNKNOWN"));
    assert!(next_text(&mut live.incoming).is_none());
}
