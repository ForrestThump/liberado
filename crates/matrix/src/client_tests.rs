use super::{MatrixClient, member_present};
use crate::messages::{MsgKind, edit_content, message_content};
use crate::room::direct_room_body;
use serde_json::json;
use wiremock::matchers::{method, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> MatrixClient {
    MatrixClient::new(&server.uri(), "as-token", "@liberado:matrix.homelab.local").unwrap()
}

async fn requests(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap_or_default()
}

#[tokio::test]
async fn send_sets_the_masquerade_the_txn_and_the_html_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$evt"})))
        .mount(&server)
        .await;

    let id = client(&server)
        .send_message(
            "!room:matrix.homelab.local",
            &message_content(MsgKind::Text, "hi <there>", "<p>hi &lt;there&gt;</p>"),
        )
        .await
        .unwrap();
    assert_eq!(id, "$evt");

    let got = requests(&server).await;
    let url = &got[0].url;
    let path = url.path();
    assert!(path.contains("/%21room%3Amatrix.homelab.local/send/m.room.message/"));
    assert!(path.split('/').next_back().unwrap().len() >= 26);
    assert_eq!(
        url.query(),
        Some("user_id=%40liberado%3Amatrix.homelab.local")
    );
    assert_eq!(
        got[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer as-token"
    );
    let body: serde_json::Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(body["body"], "hi <there>");
    assert_eq!(body["format"], "org.matrix.custom.html");
    assert_eq!(body["formatted_body"], "<p>hi &lt;there&gt;</p>");
}

#[tokio::test]
async fn edit_and_typing_use_encoded_ids() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$edit"})))
        .mount(&server)
        .await;
    let matrix = client(&server);
    matrix
        .send_message("!r:s", &edit_content("$orig", "new", "<p>new</p>"))
        .await
        .unwrap();
    matrix.set_typing("!r:s", true).await.unwrap();

    let got = requests(&server).await;
    let edit: serde_json::Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(edit["m.relates_to"]["rel_type"], "m.replace");
    assert_eq!(edit["m.relates_to"]["event_id"], "$orig");
    assert_eq!(edit["m.new_content"]["body"], "new");
    assert!(
        got[1]
            .url
            .path()
            .contains("/typing/%40liberado%3Amatrix.homelab.local")
    );
    let typing: serde_json::Value = serde_json::from_slice(&got[1].body).unwrap();
    assert_eq!(typing["typing"], true);
}

#[tokio::test]
async fn create_room_sends_the_encryption_lock() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id": "!dm:s"})))
        .mount(&server)
        .await;
    let room = client(&server)
        .create_direct_room("Coder", "@shiloh:s", "@shiloh:s")
        .await
        .unwrap();
    assert_eq!(room, "!dm:s");
    let got = requests(&server).await;
    let body: serde_json::Value = serde_json::from_slice(&got[0].body).unwrap();
    assert_eq!(
        body,
        direct_room_body(
            "Coder",
            "@shiloh:s",
            "@shiloh:s",
            "@liberado:matrix.homelab.local"
        )
    );
}

#[tokio::test]
async fn errors_omit_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("user_id", "@liberado:matrix.homelab.local"))
        .respond_with(ResponseTemplate::new(401).set_body_string("token as-token rejected"))
        .mount(&server)
        .await;
    let error = client(&server).whoami().await.unwrap_err().to_string();
    assert!(error.contains("401"));
    assert!(!error.contains("as-token"));
    assert!(member_present(
        &["@Liberado:example".into()],
        "@liberado:example"
    ));
}

#[tokio::test]
async fn whoami_and_members_read_the_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user_id": "@liberado:matrix.homelab.local",
            "joined": { "@liberado:matrix.homelab.local": {}, "@shiloh:s": {} }
        })))
        .mount(&server)
        .await;
    let matrix = client(&server);
    assert_eq!(
        matrix.whoami().await.unwrap(),
        "@liberado:matrix.homelab.local"
    );
    let members = matrix.joined_members("!r:s").await.unwrap();
    assert!(member_present(&members, "@liberado:matrix.homelab.local"));
}
