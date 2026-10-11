use std::time::Duration;

use liberado_messaging::{InboundEvent, MessagingChannel};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::{AgentDirectory, AgentSeat, display_name, reconcile_agents};
use crate::channel::MatrixChannel;
use crate::client::MatrixClient;
use crate::filter::in_user_namespace;
use crate::hub::RoomQueues;
use crate::slug::puppet_mxid;

const SERVER: &str = "matrix.homelab.local";
const OWNER: &str = "@shiloh:matrix.homelab.local";

fn seat(id: &str, title: &str) -> AgentSeat {
    AgentSeat {
        session_id: id.to_string(),
        title: title.to_string(),
    }
}

async fn homeserver() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/register"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "errcode": "M_USER_IN_USE",
            "error": "User ID already taken."
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/createRoom"))
        .respond_with(|request: &Request| {
            let query = request.url.query().unwrap_or("room");
            ResponseTemplate::new(200).set_body_json(json!({
                "room_id": format!("!{query}:{SERVER}")
            }))
        })
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "joined": {} })))
        .mount(&server)
        .await;
    server
}

fn client(server: &MockServer) -> MatrixClient {
    MatrixClient::new(&server.uri(), "as-token", "@liberado:matrix.homelab.local").expect("client")
}

async fn count(server: &MockServer, needle: &str) -> usize {
    server
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|request| request.url.path().contains(needle))
        .count()
}

fn assert_clean(label: &str, errors: &[String]) {
    assert!(errors.is_empty(), "{label}: {errors:?}");
}

#[test]
fn a_blank_title_displays_as_agent() {
    assert_eq!(display_name("  Coder  "), "Coder");
    assert_eq!(display_name("   "), "Agent");
}

#[tokio::test]
async fn directory_keeps_the_slug_across_a_reload() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let path = dir.path().join("matrix-agents.json");
    let server = homeserver().await;
    let mut directory = AgentDirectory::load(&path).await;
    let report = reconcile_agents(
        &client(&server),
        SERVER,
        OWNER,
        &mut directory,
        &[seat("session-1", "Coder")],
    )
    .await;
    assert_clean("first", &report.errors);
    let saved = AgentDirectory::load(&path).await;
    let record = saved.get("session-1").expect("saved");
    assert_eq!(record.slug, "coder");
    assert!(record.room_id.is_some());
    assert!(in_user_namespace(&record.mxid, SERVER));
    Ok(())
}

#[tokio::test]
async fn second_reconcile_creates_nothing_and_rename_keeps_the_slug() -> Result<(), String> {
    let server = homeserver().await;
    let mut directory = AgentDirectory::memory();
    let original = seat("session-1", "Coder");
    let first = reconcile_agents(
        &client(&server),
        SERVER,
        OWNER,
        &mut directory,
        std::slice::from_ref(&original),
    )
    .await;
    assert_clean("first", &first.errors);
    let mxid = first.ensured[0].mxid.clone();
    let room = first.ensured[0].room_id.clone();
    assert_eq!(first.ensured[0].slug, "coder");
    assert_eq!(mxid, puppet_mxid("coder", SERVER));
    assert_eq!(count(&server, "createRoom").await, 1);
    assert_eq!(count(&server, "register").await, 1);
    assert!(count(&server, "m.direct").await >= 1);

    let second = reconcile_agents(
        &client(&server),
        SERVER,
        OWNER,
        &mut directory,
        std::slice::from_ref(&original),
    )
    .await;
    assert_clean("second", &second.errors);
    assert_eq!(count(&server, "createRoom").await, 1, "no second room");
    assert_eq!(second.ensured[0].slug, "coder");
    assert_eq!(second.ensured[0].mxid, mxid);
    assert_eq!(second.ensured[0].room_id, room);
    let names_before_rename = count(&server, "displayname").await;

    let third = reconcile_agents(
        &client(&server),
        SERVER,
        OWNER,
        &mut directory,
        &[seat("session-1", "Coder Prime")],
    )
    .await;
    assert_clean("rename", &third.errors);
    assert_eq!(third.ensured[0].slug, "coder");
    assert_eq!(third.ensured[0].mxid, mxid);
    assert_eq!(third.ensured[0].room_id, room);
    assert_eq!(third.ensured[0].display_name, "Coder Prime");
    assert_eq!(count(&server, "createRoom").await, 1);
    assert!(count(&server, "displayname").await > names_before_rename);
    assert!(count(&server, "m.room.name").await >= 1);

    let gone = reconcile_agents(&client(&server), SERVER, OWNER, &mut directory, &[]).await;
    assert_clean("archive", &gone.errors);
    assert_eq!(gone.deactivated.len(), 1);
    assert!(!gone.deactivated[0].active);
    assert!(gone.deactivated[0].departed);
    assert_eq!(gone.deactivated[0].slug, "coder");
    assert_eq!(gone.deactivated[0].room_id, room);
    assert!(count(&server, "/leave").await >= 1 || count(&server, "leave").await >= 1);

    let back = reconcile_agents(&client(&server), SERVER, OWNER, &mut directory, &[original]).await;
    assert_clean("restore", &back.errors);
    assert!(back.ensured[0].active);
    assert_eq!(back.ensured[0].slug, "coder");
    assert_eq!(back.ensured[0].room_id, room);
    assert_eq!(count(&server, "createRoom").await, 1);
    assert!(
        count(&server, "/join/").await >= 1,
        "restore joins the puppet"
    );
    Ok(())
}

#[tokio::test]
async fn colliding_titles_get_distinct_puppet_users() -> Result<(), String> {
    let server = homeserver().await;
    let mut directory = AgentDirectory::memory();
    let report = reconcile_agents(
        &client(&server),
        SERVER,
        OWNER,
        &mut directory,
        &[seat("one", "Coder"), seat("two", "coder!")],
    )
    .await;
    assert_clean("pair", &report.errors);
    assert_eq!(report.ensured.len(), 2);
    let mut slugs: Vec<&str> = report
        .ensured
        .iter()
        .map(|agent| agent.slug.as_str())
        .collect();
    slugs.sort();
    assert_eq!(slugs, ["coder", "coder_2"]);
    for agent in &report.ensured {
        assert!(in_user_namespace(&agent.mxid, SERVER));
        assert!(agent.room_id.is_some());
    }
    assert_ne!(report.ensured[0].room_id, report.ensured[1].room_id);
    assert_eq!(count(&server, "createRoom").await, 2);
    assert_eq!(count(&server, "register").await, 2);
    Ok(())
}

#[tokio::test]
async fn each_room_speaks_and_hears_as_its_own_puppet() -> Result<(), String> {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"event_id": "$evt"})))
        .mount(&server)
        .await;
    let queues = RoomQueues::new();
    let base = MatrixClient::new(&server.uri(), "as-token", "@liberado:matrix.homelab.local")?;
    let coder = MatrixChannel::open(
        base.for_user("@liberado_coder:matrix.homelab.local"),
        "!coder:matrix.homelab.local",
        &queues,
    );
    let researcher = MatrixChannel::open(
        base.for_user("@liberado_researcher:matrix.homelab.local"),
        "!research:matrix.homelab.local",
        &queues,
    );
    coder
        .send_text("from coder")
        .await
        .map_err(|error| error.to_string())?;
    researcher
        .set_typing()
        .await
        .map_err(|error| error.to_string())?;
    queues.push(
        "!coder:matrix.homelab.local",
        InboundEvent::Message {
            text: "hello coder".into(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
    queues.push(
        "!research:matrix.homelab.local",
        InboundEvent::Message {
            text: "hello research".into(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
    queues.push(
        "!unbound:matrix.homelab.local",
        InboundEvent::Message {
            text: "ignore me".into(),
            reply_to_prompt: None,
            from_bot: false,
        },
    );
    let mut cursor = String::new();
    let coder_in = tokio::time::timeout(Duration::from_secs(2), coder.receive(&mut cursor))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let research_in = tokio::time::timeout(Duration::from_secs(2), researcher.receive(&mut cursor))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    assert_eq!(message_text(&coder_in), "hello coder");
    assert_eq!(message_text(&research_in), "hello research");

    let got = server.received_requests().await.expect("requests");
    assert_eq!(
        got[0].url.query(),
        Some("user_id=%40liberado_coder%3Amatrix.homelab.local")
    );
    assert!(
        got[1]
            .url
            .path()
            .contains("/typing/%40liberado_researcher%3Amatrix.homelab.local")
    );
    assert_eq!(
        got[1].url.query(),
        Some("user_id=%40liberado_researcher%3Amatrix.homelab.local")
    );
    Ok(())
}

fn message_text(events: &[InboundEvent]) -> &str {
    match &events[0] {
        InboundEvent::Message { text, .. } => text,
        InboundEvent::Action { action, .. } => action,
    }
}
