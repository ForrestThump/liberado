use super::{
    already_invited, already_left, register_or_existing, set_direct_room, set_display_name,
    set_room_name, user_in_use,
};
use crate::client::MatrixClient;
use liberado_messaging::MessagingError;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer, user: &str) -> MatrixClient {
    MatrixClient::new(&server.uri(), "as-token", user).expect("client")
}

async fn body_of(server: &MockServer) -> serde_json::Value {
    let got = server.received_requests().await.expect("requests");
    serde_json::from_slice(&got[0].body).expect("json")
}

#[tokio::test]
async fn register_sends_the_application_service_login_and_no_masquerade() -> Result<(), String> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user_id": "@liberado_coder:matrix.homelab.local"
        })))
        .mount(&server)
        .await;
    register_or_existing(
        &client(&server, "@liberado:matrix.homelab.local"),
        "liberado_coder",
    )
    .await
    .map_err(|error| error.to_string())?;
    let got = server.received_requests().await.expect("requests");
    assert!(
        got[0].url.query().is_none(),
        "registration must not send user_id"
    );
    let body = body_of(&server).await;
    assert_eq!(body["type"], "m.login.application_service");
    assert_eq!(body["username"], "liberado_coder");
    assert_eq!(
        got[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer as-token")
    );
    Ok(())
}

#[tokio::test]
async fn user_in_use_is_the_existing_account() -> Result<(), String> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/register"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errcode": "M_USER_IN_USE",
            "error": "User ID already taken."
        })))
        .mount(&server)
        .await;
    let matrix = client(&server, "@liberado:matrix.homelab.local");
    let error = matrix
        .post_service(
            &["register"],
            &json!({"type": "m.login.application_service", "username": "liberado_coder"}),
        )
        .await
        .expect_err("in use");
    assert!(user_in_use(&error));
    register_or_existing(&matrix, "liberado_coder")
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn profile_direct_and_room_name_masquerade_as_the_puppet() -> Result<(), String> {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let puppet = client(&server, "@liberado_coder:matrix.homelab.local");
    set_display_name(&puppet, "Coder")
        .await
        .map_err(|error| error.to_string())?;
    set_direct_room(
        &puppet,
        "@shiloh:matrix.homelab.local",
        "!dm:matrix.homelab.local",
    )
    .await
    .map_err(|error| error.to_string())?;
    set_room_name(&puppet, "!dm:matrix.homelab.local", "Coder")
        .await
        .map_err(|error| error.to_string())?;

    let got = server.received_requests().await.expect("requests");
    assert_eq!(got.len(), 3);
    assert!(got[0].url.path().contains("/displayname"));
    assert_eq!(
        got[0].url.query(),
        Some("user_id=%40liberado_coder%3Amatrix.homelab.local")
    );
    let name: serde_json::Value = serde_json::from_slice(&got[0].body).expect("name");
    assert_eq!(name["displayname"], "Coder");
    assert!(got[1].url.path().contains("/account_data/m.direct"));
    let direct: serde_json::Value = serde_json::from_slice(&got[1].body).expect("direct");
    assert_eq!(
        direct["@shiloh:matrix.homelab.local"][0],
        "!dm:matrix.homelab.local"
    );
    assert!(got[2].url.path().contains("/state/m.room.name"));
    let room: serde_json::Value = serde_json::from_slice(&got[2].body).expect("room");
    assert_eq!(room["name"], "Coder");
    Ok(())
}

#[test]
fn leave_and_invite_errors_that_mean_already_done() {
    let gone = MessagingError::new("matrix http 403 M_FORBIDDEN");
    let missing = MessagingError::new("matrix http 404 M_NOT_FOUND");
    let other = MessagingError::new("matrix http 500");
    assert!(already_left(&gone));
    assert!(already_left(&missing));
    assert!(!already_left(&other));
    assert!(already_invited(&MessagingError::new(
        "matrix http 403 M_FORBIDDEN already in the room"
    )));
    assert!(!already_invited(&other));
}
