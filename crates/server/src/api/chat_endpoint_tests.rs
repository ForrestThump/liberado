//! Split from `chat.rs` for module-health boundaries.

use super::*;
use axum::http::header::CONTENT_TYPE;
use liberado_executor::{Budget, Executor, ToolRuntime};
use liberado_main_agent::ChatSessions;
use liberado_session_store::SessionStore;
use std::sync::Arc;

struct NoTools;

#[async_trait::async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<liberado_provider::ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _: &liberado_provider::ToolInvocation) -> Result<String, String> {
        Err("no tools".into())
    }
}

async fn chat_state() -> Arc<AppState> {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(liberado_provider::MockProvider::new("m"));
    let executor = Executor::new(provider, Budget::default());
    let chat = Arc::new(ChatSessions::new(
        store.clone(),
        executor,
        Arc::new(NoTools),
    ));
    Arc::new(AppState::for_test(
        store,
        Some(chat),
        dir.path().to_path_buf(),
    ))
}

async fn disabled_state(dir: &std::path::Path) -> Arc<AppState> {
    let store = SessionStore::open(dir).await;
    Arc::new(AppState::for_test(Arc::new(store), None, dir.to_path_buf()))
}

/// Read SSE frames until every needle appears (or a deadline passes) — keep-alive comments
/// mean these bodies are not required to reach EOF, so waiting for it would hang.
async fn body_with(response: axum::response::Response, needles: &[&str]) -> String {
    use futures::StreamExt;
    if needles.is_empty() {
        use http_body_util::BodyExt as _;
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("plain body collects")
            .to_bytes();
        return String::from_utf8_lossy(&bytes).into_owned();
    }
    let mut body = Box::pin(response.into_body().into_data_stream());
    let mut seen = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if needles.iter().all(|n| seen.contains(n)) {
            break;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(500), body.as_mut().next())
            .await
        {
            Ok(Some(Ok(frame))) => seen.push_str(&String::from_utf8_lossy(&frame)),
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => continue,
        }
    }
    seen
}

async fn body_string(response: axum::response::Response) -> String {
    body_with(response, &[]).await
}

/// With no provider configured, both stream verbs return an SSE `failed` event — never a
/// bare 200 with no content type. A handler stubbed to `Default` would pass every other
/// assertion here and ship a dead endpoint.
#[tokio::test]
async fn streaming_without_chat_fails_as_an_sse_event_on_both_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let state = disabled_state(dir.path()).await;

    let make_request = || ChatRequest {
        message: "hi".into(),
        session: None,
        incognito: false,
        background: false,
        profile: None,
        model: None,
    };
    for response in [
        chat_stream_post(State(state.clone()), Json(make_request())).await,
        chat_stream_get(State(state.clone()), Query(make_request())).await,
    ] {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/event-stream",
            "a failed start is still an SSE stream"
        );
        let body = body_string(response).await;
        assert!(body.contains("chat is disabled"), "{body}");
        assert!(
            body.contains("event: failed"),
            "the failure rides the converged vocabulary: {body}"
        );
        assert!(
            body.contains('\u{2014}'),
            "disabled-chat error must carry a UTF-8 em-dash, not Windows-1252 mojibake: {body}"
        );
        assert!(
            !body.contains('\u{00e2}'),
            "U+00E2 is the first character of the Windows-1252 misread of U+2014: {body}"
        );
        let data = body
            .lines()
            .find_map(|line| {
                line.strip_prefix("data:")
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .expect("SSE failed event carries a data line");
        let event = chat_client_contract::SessionEvent::from_sse_data("failed", data)
            .expect("failed payload must decode");
        match event.kind {
            chat_client_contract::SessionEventKind::Failed { message } => {
                assert_eq!(message, super::CHAT_DISABLED_HINT);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}

/// Attach and cancel on a conversation with no running turn answer 409 (400 for a malformed
/// id) — never a silent 200.
#[tokio::test]
async fn attach_and_cancel_answer_conflict_for_an_idle_conversation() {
    let state = chat_state().await;
    let bad = "not-a-conversation-id".to_string();
    let good = Ulid::new().to_string();

    // With chat disabled, both endpoints answer the shared 503 before anything else.
    let dir = tempfile::tempdir().unwrap();
    let off = disabled_state(dir.path()).await;
    for id in [&bad, &good] {
        let attached = attach_conversation(State(off.clone()), Path(id.to_string())).await;
        assert_eq!(
            attached.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "attach off"
        );
        assert!(
            body_with(attached, &["chat is disabled"])
                .await
                .contains("chat is disabled")
        );
        let cancelled = cancel_conversation_turn(State(off.clone()), Path(id.to_string())).await;
        assert_eq!(
            cancelled.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "cancel off"
        );
    }

    for id in [&bad, &good] {
        let attached = attach_conversation(State(state.clone()), Path(id.to_string())).await;
        assert_eq!(
            attached.status(),
            if *id == bad {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
            "attach({id})"
        );
        let cancelled = cancel_conversation_turn(State(state.clone()), Path(id.to_string())).await;
        assert_eq!(
            cancelled.status(),
            if *id == bad {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::CONFLICT
            },
            "cancel({id})"
        );
    }
}

#[tokio::test]
async fn every_agent_event_maps_onto_the_wire_vocabulary() {
    let cases: Vec<(AgentEvent, &str, serde_json::Value)> = vec![
        (
            AgentEvent::Token("partial answer".into()),
            "token",
            serde_json::Value::String("partial answer".into()),
        ),
        (
            AgentEvent::ToolStarted {
                name: "edit_file".into(),
                args: "{\"path\":\"x\"}".into(),
            },
            "tool_started",
            serde_json::json!({"name":"edit_file","args_preview":"{\"path\":\"x\"}"}),
        ),
        (
            AgentEvent::ToolFinished {
                name: "edit_file".into(),
                ok: false,
                preview: "boom".into(),
            },
            "tool_finished",
            serde_json::json!({"name":"edit_file","ok":false,"result_preview":"boom"}),
        ),
        (
            AgentEvent::Done,
            "session_finished",
            serde_json::json!({"status":"done","summary":""}),
        ),
        (
            AgentEvent::Error("broken".into()),
            "failed",
            serde_json::json!({"message":"broken"}),
        ),
    ];
    use axum::response::sse::Sse;
    for (event, expected_name, expected_data) in cases {
        let rendered = to_sse(event);
        let stream = Sse::new(futures::stream::once(async move {
            Ok::<_, Infallible>(rendered)
        }));
        let response = stream.into_response();
        let body = body_with(response, &[&format!("event: {expected_name}")]).await;
        assert!(
            body.contains(&format!("event: {expected_name}")),
            "expected event [{expected_name}] in:\n{body}"
        );
        // Plain-string payloads render unquoted; JSON objects render compact.
        let needle = expected_data
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| expected_data.to_string());
        assert!(body.contains(&needle), "expected {needle} in:\n{body}");
    }
}

/// The transcript converter keeps tool calls only when they exist — flipped, every message
/// would carry `tool_calls: null`-shaped noise or drop real ones.
#[test]
fn chat_message_carries_tool_calls_only_when_present() {
    let make_node = |tool_calls: Vec<liberado_provider::ToolInvocation>| {
        liberado_conversation_store::MessageNode {
            id: Ulid::new(),
            parent_id: None,
            conversation_id: Ulid::new(),
            author: liberado_conversation_store::Author::Assistant,
            created_at: chrono::Utc::now(),
            message: liberado_provider::Message {
                role: liberado_provider::Role::Assistant,
                content: "answer".into(),
                tool_calls,
                tool_call_id: None,
            },
            model: None,
        }
    };
    let plain = chat_message_from_node(make_node(Vec::new()));
    assert!(plain.tool_calls.is_none(), "no calls, no field");

    let with_call =
        chat_message_from_node(make_node(vec![liberado_provider::ToolInvocation::new(
            "t1",
            "read_file",
            serde_json::json!({"path":"x"}),
        )]));
    let carried = with_call.tool_calls.expect("calls are carried");
    assert_eq!(carried[0]["name"], "read_file");
}

/// A store error maps onto the shared 500 JSON shape.
#[tokio::test]
async fn chat_error_is_a_json_500() {
    let err = liberado_main_agent::SessionError::Store(
        liberado_conversation_store::StoreError::NotFound(Ulid::new().to_string()),
    );
    let response = chat_error(err);
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_string(response).await;
    assert!(body.contains("error"), "{body}");
}
