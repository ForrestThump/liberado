use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::DogfoodServer;
use crate::client::DaemonClient;

fn server_for(uri: &str) -> DogfoodServer {
    DogfoodServer::from_client(DaemonClient::new(uri.to_owned(), Duration::from_secs(5)).unwrap())
}

fn parse(result: Result<String, turbomcp::McpError>) -> Value {
    let text = result.unwrap_or_else(|err| panic!("tool failed: {err}"));
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("json: {err}: {text}"))
}

async fn recorded(mock: &MockServer) -> Vec<wiremock::Request> {
    mock.received_requests().await.unwrap()
}

fn request_body(req: &wiremock::Request) -> Value {
    serde_json::from_slice(&req.body).unwrap_or(Value::Null)
}

fn profiles() -> Value {
    json!({
        "profiles": [
            {"name": "coding", "agent_eligible": true},
            {"name": "chat-default", "agent_eligible": false}
        ]
    })
}

fn mount_profiles(mock: &MockServer) -> impl std::future::Future<Output = ()> + '_ {
    Mock::given(method("GET"))
        .and(path("/api/profiles"))
        .respond_with(ResponseTemplate::new(200).set_body_json(profiles()))
        .mount(mock)
}

async fn mount_json(
    mock: &MockServer,
    http_method: &str,
    http_path: &str,
    status: u16,
    body: Value,
) {
    Mock::given(method(http_method))
        .and(path(http_path))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(mock)
        .await;
}

#[tokio::test]
async fn create_agent_one_turn_read_reply_then_continue() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(&mock, "GET", "/api/conversations", 200, json!([])).await;
    mount_json(
        &mock,
        "POST",
        "/api/conversations",
        201,
        json!({
            "id": "01AGENT",
            "title": "Budget",
            "created_at": "2026-10-06T00:00:00Z",
            "surface_mode": "agent",
            "grant": {"profile": "coding"}
        }),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_json(json!({"session": "01AGENT", "message": "hello"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "reply": "first reply",
            "session": "01AGENT"
        })))
        .mount(&mock)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_json(
            json!({"session": "01AGENT", "message": "and then"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "reply": "second reply",
            "session": "01AGENT"
        })))
        .mount(&mock)
        .await;
    mount_json(
        &mock,
        "GET",
        "/api/conversations/01AGENT",
        200,
        json!({
            "messages": [
                {"role": "system", "content": "prompt"},
                {"role": "assistant", "content": "old"},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "newer reply"}
            ],
            "profile": "coding",
            "surface_mode": "agent",
            "turn_running": false,
            "turn_unanswered": false
        }),
    )
    .await;

    let server = server_for(&mock.uri());
    let created = parse(
        server
            .create_agent("coding".into(), Some("Budget".into()))
            .await,
    );
    assert_eq!(created["conversation_id"], "01AGENT");
    assert_eq!(created["reused"], false);
    assert_eq!(created["surface_mode"], "agent");
    assert_eq!(created["profile"], "coding");

    let first = parse(
        server
            .send_human_message("01AGENT".into(), "hello".into())
            .await,
    );
    assert_eq!(first["reply"], "first reply");
    assert_eq!(first["session"], "01AGENT");

    let history = parse(server.read_replies("01AGENT".into()).await);
    assert_eq!(history["reply"], "newer reply");
    assert_eq!(history["turn_running"], false);
    assert_eq!(history["surface_mode"], "agent");

    let second = parse(
        server
            .continue_session("01AGENT".into(), "and then".into())
            .await,
    );
    assert_eq!(second["reply"], "second reply");
    assert_eq!(second["session"], "01AGENT");

    let reqs = recorded(&mock).await;
    assert!(
        reqs.iter()
            .all(|req| req.headers.get("authorization").is_none()),
        "dogfood client must not invent an Authorization header"
    );
    let creates: Vec<_> = reqs
        .iter()
        .filter(|req| req.method.as_str() == "POST" && req.url.path() == "/api/conversations")
        .collect();
    assert_eq!(creates.len(), 1);
    let create_body = request_body(creates[0]);
    assert_eq!(create_body, json!({"profile": "coding", "title": "Budget"}));
    assert!(create_body.get("agent_creator").is_none());
    let turns: Vec<_> = reqs
        .iter()
        .filter(|req| req.method.as_str() == "POST" && req.url.path() == "/api/chat")
        .map(request_body)
        .collect();
    assert_eq!(
        turns,
        vec![
            json!({"session": "01AGENT", "message": "hello"}),
            json!({"session": "01AGENT", "message": "and then"}),
        ]
    );
    assert!(
        turns.iter().all(|body| body["message"] != "newer reply"),
        "assistant text must not be posted back as a human turn"
    );
}

#[tokio::test]
async fn create_agent_reuses_the_oldest_row_and_does_not_post() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(
        &mock,
        "GET",
        "/api/conversations",
        200,
        json!([
            {
                "id": "NEW",
                "title": "Budget",
                "created_at": "2026-04-01T00:00:00Z",
                "surface_mode": "agent",
                "grant": {"profile": "coding"}
            },
            {
                "id": "OLD",
                "title": "Budget",
                "created_at": "2026-01-01T00:00:00Z",
                "surface_mode": "agent",
                "grant": {"profile": "coding"}
            }
        ]),
    )
    .await;
    let server = server_for(&mock.uri());
    let created = parse(
        server
            .create_agent(" coding ".into(), Some(" Budget ".into()))
            .await,
    );
    assert_eq!(created["conversation_id"], "OLD");
    assert_eq!(created["reused"], true);
    assert!(
        recorded(&mock)
            .await
            .iter()
            .all(|req| req.method.as_str() != "POST")
    );
}

#[tokio::test]
async fn ineligible_profile_does_not_post_a_conversation() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    let server = server_for(&mock.uri());
    let err = server
        .create_agent("chat-default".into(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("agent_eligible"), "{err}");
    assert!(
        recorded(&mock)
            .await
            .iter()
            .all(|req| req.method.as_str() != "POST")
    );
}

#[tokio::test]
async fn a_chat_row_with_the_same_title_is_not_reused() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(
        &mock,
        "GET",
        "/api/conversations",
        200,
        json!([{
            "id": "CHAT",
            "title": "Budget",
            "created_at": "2026-01-01T00:00:00Z",
            "surface_mode": "chat",
            "grant": {"profile": "coding"}
        }]),
    )
    .await;
    mount_json(
        &mock,
        "POST",
        "/api/conversations",
        201,
        json!({
            "id": "AGENT",
            "title": "Budget",
            "surface_mode": "agent",
            "grant": {"profile": "coding"}
        }),
    )
    .await;
    let created = parse(
        server_for(&mock.uri())
            .create_agent("coding".into(), Some("Budget".into()))
            .await,
    );
    assert_eq!(created["conversation_id"], "AGENT");
    assert_eq!(created["reused"], false);
}

#[tokio::test]
async fn creator_singleton_is_not_reused_as_the_named_agent() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(
        &mock,
        "GET",
        "/api/conversations",
        200,
        json!([{
            "id": "CREATOR",
            "title": "Budget",
            "created_at": "2026-01-01T00:00:00Z",
            "surface_mode": "agent",
            "agent_creator": true,
            "grant": {"profile": "coding"}
        }]),
    )
    .await;
    mount_json(
        &mock,
        "POST",
        "/api/conversations",
        201,
        json!({
            "id": "AGENT",
            "title": "Budget",
            "surface_mode": "agent",
            "grant": {"profile": "coding"}
        }),
    )
    .await;
    let created = parse(
        server_for(&mock.uri())
            .create_agent("coding".into(), Some("Budget".into()))
            .await,
    );
    assert_eq!(created["conversation_id"], "AGENT");
}

#[tokio::test]
async fn chat_stamp_from_create_is_an_error() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(&mock, "GET", "/api/conversations", 200, json!([])).await;
    mount_json(
        &mock,
        "POST",
        "/api/conversations",
        201,
        json!({"id": "STRAY", "title": "Budget", "surface_mode": "chat"}),
    )
    .await;
    let err = server_for(&mock.uri())
        .create_agent("coding".into(), Some("Budget".into()))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("surface_mode"), "{text}");
    assert!(text.contains("STRAY"), "{text}");
}

#[tokio::test]
async fn blank_title_is_sent_as_the_profile_name() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(&mock, "GET", "/api/conversations", 200, json!([])).await;
    mount_json(
        &mock,
        "POST",
        "/api/conversations",
        201,
        json!({
            "id": "AGENT",
            "title": "coding",
            "surface_mode": "agent",
            "grant": {"profile": "coding"}
        }),
    )
    .await;
    let created = parse(
        server_for(&mock.uri())
            .create_agent("coding".into(), None)
            .await,
    );
    assert_eq!(created["title"], "coding");
    let reqs = recorded(&mock).await;
    let body = request_body(
        reqs.iter()
            .find(|req| req.method.as_str() == "POST")
            .unwrap(),
    );
    assert_eq!(body, json!({"profile": "coding", "title": "coding"}));
}

#[tokio::test]
async fn empty_profile_and_empty_human_message_do_not_call_the_daemon() {
    let mock = MockServer::start().await;
    let server = server_for(&mock.uri());
    let err = server.create_agent("  ".into(), None).await.unwrap_err();
    assert!(err.to_string().contains("non-empty"), "{err}");
    let err = server
        .send_human_message("01AGENT".into(), "  ".into())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not invent"), "{err}");
    assert!(recorded(&mock).await.is_empty());
}

#[tokio::test]
async fn start_chat_posts_an_empty_body_and_refuses_an_agent_profile() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    mount_json(
        &mock,
        "POST",
        "/api/conversations",
        201,
        json!({
            "id": "CHAT",
            "title": null,
            "surface_mode": "chat"
        }),
    )
    .await;
    let server = server_for(&mock.uri());
    let created = parse(server.start_chat(None, None).await);
    assert_eq!(created["conversation_id"], "CHAT");
    assert_eq!(created["surface_mode"], "chat");
    let opens: Vec<_> = recorded(&mock)
        .await
        .into_iter()
        .filter(|req| req.method.as_str() == "POST")
        .collect();
    assert_eq!(opens.len(), 1);
    assert_eq!(request_body(&opens[0]), json!({}));

    let err = server
        .start_chat(Some("coding".into()), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("create_agent"), "{err}");
}

#[tokio::test]
async fn start_chat_sends_a_non_agent_profile() {
    let mock = MockServer::start().await;
    mount_profiles(&mock).await;
    Mock::given(method("POST"))
        .and(path("/api/conversations"))
        .and(body_json(
            json!({"profile": "chat-default", "title": "Notes"}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": "CHAT",
            "title": "Notes",
            "surface_mode": "chat",
            "grant": {"profile": "chat-default"}
        })))
        .expect(1)
        .mount(&mock)
        .await;
    let created = parse(
        server_for(&mock.uri())
            .start_chat(Some("chat-default".into()), Some("Notes".into()))
            .await,
    );
    assert_eq!(created["profile"], "chat-default");
    assert_eq!(created["surface_mode"], "chat");
}

#[tokio::test]
async fn daemon_error_json_is_surfaced_and_redirects_are_not_followed() {
    let mock = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "chat disabled"})))
        .mount(&mock)
        .await;
    let err = server_for(&mock.uri())
        .continue_session("01AGENT".into(), "hello".into())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("chat disabled"), "{err}");

    let redirect = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/status"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "file:///etc/passwd"))
        .mount(&redirect)
        .await;
    let err = server_for(&redirect.uri()).status().await.unwrap_err();
    assert!(err.to_string().contains("302"), "{err}");
    assert_eq!(recorded(&redirect).await.len(), 1);
}

#[tokio::test]
async fn lists_and_workspace_smoke_are_read_only() {
    let mock = MockServer::start().await;
    mount_json(
        &mock,
        "GET",
        "/api/conversations",
        200,
        json!([
            {
                "id": "CREATOR",
                "title": "Agent Creator",
                "created_at": "2026-01-01T00:00:00Z",
                "surface_mode": "agent",
                "agent_creator": true,
                "grant": {"profile": "operator"}
            },
            {
                "id": "CHAT",
                "title": "Hi",
                "created_at": "2026-02-01T00:00:00Z",
                "surface_mode": "chat"
            },
            {
                "id": "AGENT",
                "title": "Budget",
                "created_at": "2026-03-01T00:00:00Z",
                "surface_mode": "agent",
                "grant": {"profile": "coding"}
            }
        ]),
    )
    .await;
    mount_json(
        &mock,
        "GET",
        "/api/sessions",
        200,
        json!([
            {"id": "CHAT", "title": "Hi", "surface_mode": "chat", "status": "running"},
            {
                "id": "GOAL",
                "title": "Ship",
                "surface_mode": "chat",
                "status": "running",
                "goal": {"description": "ship"},
                "grant": {"profile": "coding"}
            }
        ]),
    )
    .await;
    mount_json(
        &mock,
        "GET",
        "/api/status",
        200,
        json!({
            "running": true,
            "chat_tool_names": ["face:workspace_read", "workspace_list"]
        }),
    )
    .await;
    let server = server_for(&mock.uri());
    let agents = parse(server.list_agents().await);
    assert_eq!(agents["agents"].as_array().unwrap().len(), 1);
    assert_eq!(agents["agents"][0]["id"], "AGENT");
    let sessions = parse(server.list_sessions().await);
    assert_eq!(sessions["sessions"][1]["has_goal"], true);
    assert_eq!(sessions["sessions"][1]["profile"], "coding");
    let smoke = parse(server.workspace_smoke().await);
    assert_eq!(
        smoke["present"],
        json!(["workspace_list", "workspace_read"])
    );
    assert_eq!(smoke["read_only"], true);
    assert!(
        recorded(&mock)
            .await
            .iter()
            .all(|req| req.method.as_str() == "GET")
    );
}
