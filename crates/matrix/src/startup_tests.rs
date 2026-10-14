use crate::client::MatrixClient;
use crate::config::MatrixSettings;
use crate::startup::prepare_room;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const USER: &str = "@liberado:matrix.homelab.local";
const ROOM: &str = "!room:matrix.homelab.local";

fn settings(homeserver: &str, room: &str) -> MatrixSettings {
    MatrixSettings {
        homeserver: homeserver.to_string(),
        server_name: "matrix.homelab.local".into(),
        as_token: "as-token".into(),
        hs_token: "hs-token".into(),
        owner: "@shiloh:matrix.homelab.local".into(),
        main_user: USER.into(),
        main_room: room.into(),
        listen: "127.0.0.1:9".parse().unwrap(),
        as_id: "liberado".into(),
        sender_localpart: "liberado_bridge".into(),
    }
}

fn client(server: &MockServer) -> MatrixClient {
    MatrixClient::new(&server.uri(), "as-token", USER).unwrap()
}

async fn whoami(server: &MockServer, user: &str) {
    Mock::given(method("GET"))
        .and(path_regex(".*/account/whoami"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"user_id": user})))
        .mount(server)
        .await;
}

async fn members(server: &MockServer, present: bool) {
    let joined = if present {
        json!({ USER: {} })
    } else {
        json!({})
    };
    Mock::given(method("GET"))
        .and(path_regex(".*/joined_members"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"joined": joined})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn a_member_room_id_needs_no_join() {
    let server = MockServer::start().await;
    whoami(&server, USER).await;
    members(&server, true).await;
    let room = prepare_room(&client(&server), &settings(&server.uri(), ROOM))
        .await
        .unwrap();
    assert_eq!(room, ROOM);
    let posts = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.method == "POST")
        .count();
    assert_eq!(posts, 0);
}

#[tokio::test]
async fn an_alias_is_resolved_and_the_user_joins_when_absent() {
    let server = MockServer::start().await;
    whoami(&server, "").await;
    Mock::given(method("GET"))
        .and(path_regex(".*/directory/room/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id": ROOM})))
        .mount(&server)
        .await;
    members(&server, false).await;
    Mock::given(method("POST"))
        .and(path_regex(".*/join/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"room_id": ROOM})))
        .mount(&server)
        .await;
    let room = prepare_room(
        &client(&server),
        &settings(&server.uri(), "#liberado:matrix.homelab.local"),
    )
    .await
    .unwrap();
    assert_eq!(room, ROOM);
}

#[tokio::test]
async fn the_bridge_user_passes_whoami_and_an_outsider_does_not() {
    let server = MockServer::start().await;
    whoami(&server, "@liberado_bridge:matrix.homelab.local").await;
    members(&server, true).await;
    prepare_room(&client(&server), &settings(&server.uri(), ROOM))
        .await
        .unwrap();

    let other = MockServer::start().await;
    whoami(&other, "@alice:matrix.homelab.local").await;
    let error = prepare_room(&client(&other), &settings(&other.uri(), ROOM))
        .await
        .unwrap_err();
    assert!(error.contains("@alice:matrix.homelab.local"), "{error}");
    assert!(!error.contains("as-token"), "{error}");
}

#[tokio::test]
async fn a_failed_check_names_the_step_and_hides_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_string("token as-token rejected"))
        .mount(&server)
        .await;
    let error = prepare_room(&client(&server), &settings(&server.uri(), ROOM))
        .await
        .unwrap_err();
    assert!(error.contains("matrix whoami failed"), "{error}");
    assert!(!error.contains("as-token"), "{error}");
}
