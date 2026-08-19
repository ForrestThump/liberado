//! Integration tests for the Telegram transport, driven against a local wiremock server.
//!
//! The transport builds its URL from `api_base` (injected per-server), so every POST/GET is
//! captured and asserted locally — no real network, no `LIBERADO_TELEGRAM_*` env, deterministic
//! on CI. This covers the request-shaping, response-precondition, chunking, and `receive` parser
//! logic that the in-crate unit tests (pure helpers) cannot reach.

use liberado_messaging::{ActionButton, InboundEvent, MessagingChannel};
use liberado_notify::{Notifier, TelegramNotifier};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A notifier pointed at `server` with instant poll backoff (so backoff tests don't sleep).
fn notifier_at(server: &MockServer) -> TelegramNotifier {
    TelegramNotifier::new("abc123", "42")
        .with_api_base(format!("{}/bot", server.uri()))
        .with_poll_tuning(1, 0)
}

/// A notifier aimed at a closed port (127.0.0.1:1 refuses connections) — for transport-failure
/// paths without any mock server.
fn unreachable_notifier() -> TelegramNotifier {
    TelegramNotifier::new("abc123", "42")
        .with_api_base("http://127.0.0.1:1/bot")
        .with_poll_tuning(1, 0)
}

/// Rebuild the inline-keyboard shape from `ActionButton` fields — mirrors the transport's
/// encoder without needing to reach into its private helper.
fn kb(rows: &[Vec<ActionButton>]) -> serde_json::Value {
    json!({ "inline_keyboard": rows.iter().map(|row| {
        row.iter().map(|b| json!({
            "text": b.label,
            "callback_data": format!("{}:{}", b.action, b.correlation_id),
        })).collect::<Vec<_>>()
    }).collect::<Vec<_>>() })
}

#[tokio::test]
async fn send_text_posts_chat_id_and_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/sendMessage"))
        .and(body_json(json!({"chat_id": "42", "text": "hello"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server).send_text("hello").await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn send_text_chunks_overlong_messages_without_breaking_chars() {
    let server = MockServer::start().await;
    let first: String = "a".repeat(4000);
    Mock::given(method("POST"))
        .and(body_json(json!({"chat_id": "42", "text": first})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_json(json!({"chat_id": "42", "text": "bcde"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .send_text(&format!("{first}bcde"))
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn send_text_surfaces_api_error_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/sendMessage"))
        .respond_with(ResponseTemplate::new(400).set_body_string("You have no rights"))
        .expect(1)
        .mount(&server)
        .await;
    let err = notifier_at(&server).send_text("hello").await.unwrap_err();
    assert!(err.0.contains("You have no rights"), "got: {err:?}");
}

#[tokio::test]
async fn send_text_transport_failure_is_error_not_panic() {
    let err = unreachable_notifier().send_text("hello").await.unwrap_err();
    assert!(err.0.contains("Telegram request failed"), "got: {err:?}");
}

#[tokio::test]
async fn notify_delegates_to_send_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_json(json!({"chat_id": "42", "text": "odd job"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server).notify("odd job").await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn send_with_actions_sends_first_chunk_with_markup() {
    let server = MockServer::start().await;
    let rows = liberado_messaging::approval_action_rows("prop-1");
    Mock::given(method("POST"))
        .and(body_json(json!({
            "chat_id": "42",
            "text": "approve this",
            "reply_markup": kb(&rows),
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .send_with_actions("approve this", &rows)
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn send_with_actions_overlong_correlation_falls_back_to_plain_text() {
    let server = MockServer::start().await;
    let rows = liberado_messaging::approval_action_rows(&"x".repeat(51));
    Mock::given(method("POST"))
        .and(body_json(
            json!({"chat_id": "42", "text": "too long to fit"}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let notifier = notifier_at(&server);
    notifier
        .send_with_actions("too long to fit", &rows)
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn send_with_actions_buttons_on_first_chunk_followups_plain() {
    let server = MockServer::start().await;
    let rows = liberado_messaging::permission_action_rows("perm-1");
    let first: String = "a".repeat(4000);
    Mock::given(method("POST"))
        .and(body_json(json!({
            "chat_id": "42",
            "text": first.clone(),
            "reply_markup": kb(&rows),
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_json(json!({"chat_id": "42", "text": "b".to_string()})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .send_with_actions(&format!("{first}b"), &rows)
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn send_with_actions_empty_text_still_posts_first_chunk() {
    let server = MockServer::start().await;
    let rows = liberado_messaging::approval_action_rows("prop-1");
    Mock::given(method("POST"))
        .and(body_json(json!({
            "chat_id": "42",
            "text": "",
            "reply_markup": kb(&rows),
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .send_with_actions("", &rows)
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn notify_proposal_sends_approval_buttons() {
    let server = MockServer::start().await;
    let rows = liberado_messaging::approval_action_rows("prop-9");
    Mock::given(method("POST"))
        .and(body_json(json!({
            "chat_id": "42",
            "text": "new proposal",
            "reply_markup": kb(&rows),
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .notify_proposal("prop-9", "new proposal")
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn notify_permission_request_sends_scope_buttons() {
    let server = MockServer::start().await;
    let rows = liberado_messaging::permission_action_rows("perm-2");
    Mock::given(method("POST"))
        .and(body_json(json!({
            "chat_id": "42",
            "text": "needs access",
            "reply_markup": kb(&rows),
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .notify_permission_request("perm-2", "needs access")
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn request_reply_returns_the_message_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/sendMessage"))
        .and(body_json(json!({
            "chat_id": "42",
            "text": "what changed?",
            "reply_markup": {"force_reply": true, "input_field_placeholder": "Describe the changes needed..."},
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"message_id": 77}})))
        .expect(1)
        .mount(&server)
        .await;
    let id = notifier_at(&server)
        .request_reply("what changed?")
        .await
        .unwrap();
    assert_eq!(id, "77");
}

#[tokio::test]
async fn request_reply_missing_message_id_is_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/sendMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {}})))
        .expect(1)
        .mount(&server)
        .await;
    let err = notifier_at(&server).request_reply("hi").await.unwrap_err();
    assert!(err.0.contains("missing message_id"), "got: {err:?}");
}

#[tokio::test]
async fn acknowledge_posts_event_and_receipt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/answerCallbackQuery"))
        .and(body_json(
            json!({"callback_query_id": "q1", "text": "done"}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .acknowledge("q1", "done")
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn edit_message_posts_new_text_and_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/editMessageText"))
        .and(body_json(
            json!({"chat_id": "42", "message_id": 77, "text": "revised"}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .edit_message("77", "revised")
        .await
        .unwrap();
    server.verify().await;
}

#[tokio::test]
async fn edit_message_bad_ref_rejects_without_any_http() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/editMessageText"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let err = notifier_at(&server)
        .edit_message("not-a-number", "x")
        .await
        .unwrap_err();
    assert!(err.0.contains("bad message id"), "got: {err:?}");
    server.verify().await; // proves the request was never sent
}

#[tokio::test]
async fn set_typing_posts_action_and_is_best_effort() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/sendChatAction"))
        .and(body_json(json!({"chat_id": "42", "action": "typing"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server).set_typing().await.unwrap();
    // Even a refused connection is swallowed — the method contract is best-effort.
    unreachable_notifier().set_typing().await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn register_commands_empty_is_a_noop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/setMyCommands"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    notifier_at(&server).register_commands(&[]).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn register_commands_posts_commands_and_chat_scope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/botabc123/setMyCommands"))
        .and(body_json(json!({
            "commands": [{"command": "approve", "description": "Approve"}, {"command": "reject", "description": "Reject"}],
            "scope": {"type": "chat", "chat_id": "42"},
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    notifier_at(&server)
        .register_commands(&[
            ("approve".into(), "Approve".into()),
            ("reject".into(), "Reject".into()),
        ])
        .await
        .unwrap();
    server.verify().await;
}

// ── receive (getUpdates long-poll parser) ─────────────────────────────────────

#[tokio::test]
async fn receive_parses_callback_query_into_action_and_advances_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/botabc123/getUpdates"))
        .and(query_param("offset", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "update_id": 10,
                "callback_query": {
                    "id": "q1",
                    "data": "approve:prop-1",
                    "message": {"message_id": 55},
                },
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = "7".to_string();
    let events = notifier_at(&server).receive(&mut cursor).await.unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        InboundEvent::Action {
            action,
            correlation_id,
            event_id,
            message_ref,
        } => {
            assert_eq!(action, "approve");
            assert_eq!(correlation_id, "prop-1");
            assert_eq!(event_id, "q1");
            assert_eq!(message_ref.as_deref(), Some("55"));
        }
        other => panic!("expected Action, got {other:?}"),
    }
    assert_eq!(cursor, "11", "cursor advances past the served update_id");
}

#[tokio::test]
async fn receive_unparseable_cursor_defaults_to_zero_offset() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(query_param("offset", "0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = "not-a-number".to_string();
    notifier_at(&server).receive(&mut cursor).await.unwrap();
    server.verify().await;
}

#[tokio::test]
async fn receive_bad_callback_data_skips_event_but_advances_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{"update_id": 3, "callback_query": {"id": "q1", "data": "no-colon"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = String::new();
    let events = notifier_at(&server).receive(&mut cursor).await.unwrap();
    assert!(events.is_empty());
    assert_eq!(cursor, "4");
}

#[tokio::test]
async fn receive_message_from_allowed_chat_with_reply_and_bot_flag() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "update_id": 20,
                "message": {
                    "chat": {"id": 42},
                    "from": {"is_bot": false},
                    "text": "please revise",
                    "reply_to_message": {"message_id": 9},
                },
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = String::new();
    let events = notifier_at(&server).receive(&mut cursor).await.unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        InboundEvent::Message {
            text,
            reply_to_prompt,
            from_bot,
        } => {
            assert_eq!(text, "please revise");
            assert_eq!(reply_to_prompt.as_deref(), Some("9"));
            assert!(!from_bot);
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[tokio::test]
async fn receive_accepts_string_chat_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "update_id": 30,
                "message": {"chat": {"id": "42"}, "from": {"is_bot": true}, "text": "bot says hi"},
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = String::new();
    let events = notifier_at(&server).receive(&mut cursor).await.unwrap();
    match &events[0] {
        InboundEvent::Message { text, from_bot, .. } => {
            assert_eq!(text, "bot says hi");
            assert!(*from_bot);
        }
        other => panic!("expected Message, got {other:?}"),
    }
}

#[tokio::test]
async fn receive_ignores_message_from_foreign_chat() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "result": [{
                "update_id": 40,
                "message": {"chat": {"id": 999}, "from": {"is_bot": false}, "text": "intruder"},
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = String::new();
    let events = notifier_at(&server).receive(&mut cursor).await.unwrap();
    assert!(events.is_empty(), "foreign chat must be filtered out");
    assert_eq!(
        cursor, "41",
        "cursor still advances past the filtered update"
    );
}

#[tokio::test]
async fn receive_non_success_response_backs_off_and_returns_empty() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/botabc123/getUpdates"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;
    let mut cursor = String::new();
    let events = notifier_at(&server).receive(&mut cursor).await.unwrap();
    assert!(events.is_empty());
}

#[tokio::test]
async fn receive_transport_failure_backs_off_and_returns_empty() {
    let mut cursor = String::new();
    let events = unreachable_notifier().receive(&mut cursor).await.unwrap();
    assert!(events.is_empty());
}
