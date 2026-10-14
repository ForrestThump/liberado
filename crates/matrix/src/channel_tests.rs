use std::sync::Arc;

use liberado_messaging::InboundEvent;
use liberado_messaging::MessagingChannel;
use liberado_messaging::ResolvedElsewhere;
use liberado_messaging::permission_action_rows;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

use crate::channel::MatrixCardUpdater;
use crate::channel::MatrixChannel;
use crate::client::MatrixClient;
use crate::hub::RoomQueues;
use crate::messages::CHUNK_BYTES;

const ROOM: &str = "!room:matrix.homelab.local";
const USER: &str = "@liberado:matrix.homelab.local";

fn open(server: &MockServer) -> (RoomQueues, MatrixChannel) {
    let client = MatrixClient::new(&server.uri(), "as-token", USER).unwrap();
    let queues = RoomQueues::new();
    let channel = MatrixChannel::open(client, ROOM, &queues);
    (queues, channel)
}

async fn mount_send(server: &MockServer) {
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$evt"})))
        .mount(server)
        .await;
}

fn bodies(requests: &[wiremock::Request]) -> Vec<serde_json::Value> {
    requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

#[tokio::test]
async fn send_edit_and_typing_use_the_masquerade() {
    let server = MockServer::start().await;
    mount_send(&server).await;
    let (_queues, channel) = open(&server);
    channel.send_text("**bold**").await.unwrap();
    channel.edit_message("$evt", "next").await.unwrap();
    channel.set_typing().await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0]
            .url
            .query()
            .unwrap()
            .contains("user_id=%40liberado%3Amatrix.homelab.local")
    );
    let sent = bodies(&requests);
    assert_eq!(sent[0]["body"], "**bold**");
    assert_eq!(sent[0]["format"], "org.matrix.custom.html");
    assert!(
        sent[0]["formatted_body"]
            .as_str()
            .unwrap()
            .contains("<strong>bold</strong>")
    );
    assert_eq!(sent[1]["m.relates_to"]["rel_type"], "m.replace");
    assert_eq!(sent[1]["m.relates_to"]["event_id"], "$evt");
    assert_eq!(sent[1]["m.new_content"]["body"], "next");
    assert!(requests[2].url.path().contains("/typing/"));
}

#[tokio::test]
async fn a_long_message_is_split_into_chunks() {
    let server = MockServer::start().await;
    mount_send(&server).await;
    let (_queues, channel) = open(&server);
    let text = "a".repeat(CHUNK_BYTES + 1);
    channel.send_text(&text).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let sent = bodies(&requests);
    let joined = format!(
        "{}{}",
        sent[0]["body"].as_str().unwrap(),
        sent[1]["body"].as_str().unwrap()
    );
    assert_eq!(joined, text);
}

#[tokio::test]
async fn a_keyword_is_an_action_and_a_sentence_stays_text() {
    let server = MockServer::start().await;
    mount_send(&server).await;
    let (queues, channel) = open(&server);
    channel
        .send_with_actions("Allow this tool?", &permission_action_rows("perm-9"))
        .await
        .unwrap();
    queues.push(
        ROOM,
        InboundEvent::Message {
            text: "Once".into(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
    queues.push(
        ROOM,
        InboundEvent::Message {
            text: "status of the build".into(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
    queues.push(
        ROOM,
        InboundEvent::Message {
            text: "status".into(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
    let mut cursor = String::from("old");
    let first = channel.receive(&mut cursor).await.unwrap();
    assert_eq!(cursor, "0");
    match &first[0] {
        InboundEvent::Action {
            action,
            correlation_id,
            message_ref,
            ..
        } => {
            assert_eq!(action, "once");
            assert_eq!(correlation_id, "perm-9");
            assert_eq!(message_ref.as_deref(), Some("$evt"));
        }
        other => panic!("expected action, got {other:?}"),
    }
    let second = channel.receive(&mut cursor).await.unwrap();
    match &second[0] {
        InboundEvent::Message { text, .. } => assert_eq!(text, "status of the build"),
        other => panic!("expected text, got {other:?}"),
    }
    let third = channel.receive(&mut cursor).await.unwrap();
    match &third[0] {
        InboundEvent::Message { text, .. } => assert_eq!(text, "/status"),
        other => panic!("expected command, got {other:?}"),
    }
}

#[tokio::test]
async fn a_decision_elsewhere_edits_the_card() {
    let server = MockServer::start().await;
    mount_send(&server).await;
    let (_queues, channel) = open(&server);
    let channel = Arc::new(channel);
    channel
        .send_with_actions("Allow this tool?", &permission_action_rows("perm-9"))
        .await
        .unwrap();
    let updater = MatrixCardUpdater::new(Arc::clone(&channel));
    assert_eq!(updater.surface_id(), "matrix");
    updater
        .on_resolved_elsewhere("perm-9", "Allowed once")
        .await;
    let requests = server.received_requests().await.unwrap();
    let sent = bodies(&requests);
    assert!(sent.len() >= 2);
    assert_eq!(sent[1]["m.relates_to"]["event_id"], "$evt");
    assert_eq!(sent[1]["m.new_content"]["body"], "Allowed once");
}
