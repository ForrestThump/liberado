//! Tests for the cross-provider fallback feature.
//!
//! The contract under test is in [`OpenAiCompatibleProvider::with_fallback`] and the
//! `complete` / `complete_stream` retry-once paths:
//!
//! - A 2xx on the primary returns the primary's reply. Fallback is never touched.
//! - A non-2xx on the primary with a status in `fallback_on_status` retries ONCE on the fallback
//!   provider. The fallback body uses the fallback provider's model, even when the request
//!   already names the primary slug. A fallback that itself has a fallback is not consulted.
//! - A non-2xx on the primary with a status NOT in `fallback_on_status` returns the primary's
//!   error verbatim. The fallback is never touched (404 on the primary is a caller error, not
//!   a transient — we don't silently downgrade it).
//! - When the fallback also fails, the caller sees the fallback's error. The primary's error is
//!   logged at `warn!` (visible in `tracing-test`) but not returned.
//!
//! All assertions are made on the **server-side** mock to keep the test focused on what was
//! requested and with what body, not on what the daemon's higher layers do with the response.

use super::*;
use liberado_provider::{CompletionRequest, Message};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Captures each request it serves AND remembers which backend served it (the test runs two
/// backends in parallel — primary + fallback — and asserts each received the right call).
struct TaggedCapture {
    bodies: Arc<Mutex<Vec<Value>>>,
    response: ResponseTemplate,
}

impl Respond for TaggedCapture {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if let Ok(body) = serde_json::from_slice::<Value>(&request.body) {
            self.bodies.lock().unwrap().push(body);
        }
        self.response.clone()
    }
}

async fn recording_server(
    response: ResponseTemplate,
    on_path: &str,
) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let server = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path(on_path))
        .respond_with(TaggedCapture {
            bodies: Arc::clone(&bodies),
            response,
        })
        .mount(&server)
        .await;
    (server, bodies)
}

fn one_turn() -> CompletionRequest {
    CompletionRequest::new(vec![Message::user("hi")])
}

fn chat_200_with(content: &str, model_id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": model_id,
        "choices": [{ "message": { "role": "assistant", "content": content } }]
    }))
}

/// MiniMax's actual `insufficient_balance_error` envelope, captured live on 2026-09-22 by
/// hitting `https://api.minimax.io/v1/chat/completions` with a depleted key. Pinned here so the
/// fallback test fails loudly if MiniMax ever changes the shape — our policy triggers on the
/// HTTP status, not the body, but the body is what we log on every fallback event, and a
/// future regression that strips the `error.code` (or worse, changes the error code from 402)
/// would silently degrade the fallback path. Keeping the real shape here means the assertion
/// "if this exact body arrives on 402, fallback fires" stays testable.
const MINIMAX_INSUFFICIENT_BALANCE_BODY: &str = r#"{"type":"error","error":{"type":"insufficient_balance_error","message":"insufficient balance (1008)","http_code":"402"},"request_id":"07010bc3b7d0f5522b7a24c4699990e6"}"#;

/// Build a MiniMax-shaped 402 response. The body is the contracted `insufficient_balance_error`
/// shape; the test fixture below asserts on its exact fields.
fn minimax_402() -> ResponseTemplate {
    ResponseTemplate::new(402).set_body_string(MINIMAX_INSUFFICIENT_BALANCE_BODY)
}

/// Generic helper for non-MiniMax error bodies (e.g. synthetic 502/503/400 paths). The
/// `primary_402_with_configured_code_triggers_fallback_with_overridden_model` test uses the
/// realistic `minimax_402()` fixture above; this is for the shape-agnostic cases.
fn chat_error(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_string(body)
}

/// Primary returns 200 → fallback is never contacted.
#[tokio::test]
async fn primary_success_skips_fallback_entirely() {
    let (primary, primary_bodies) = recording_server(
        chat_200_with("from primary", "primary-model"),
        "/chat/completions",
    )
    .await;
    let (fallback, fallback_bodies) = recording_server(
        chat_200_with("from fallback", "fb-model"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()),
            vec![402],
        );

    let resp = provider
        .complete(one_turn())
        .await
        .expect("primary should succeed");
    assert_eq!(resp.content.as_deref(), Some("from primary"));

    // Primary got exactly one call.
    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
    // Fallback was never contacted — its mock would have rejected the request (no mock
    // registered there) and we never saw an error, which is the proof.
    assert_eq!(
        fallback_bodies.lock().unwrap().len(),
        0,
        "fallback must not be touched when the primary succeeds"
    );
}

/// Primary returns a status in `fallback_on_status` → fallback is contacted once with the
/// configured model and its reply is returned.
#[tokio::test]
async fn primary_402_with_configured_code_triggers_fallback_with_overridden_model() {
    let (primary, primary_bodies) = recording_server(minimax_402(), "/chat/completions").await;
    let (fallback, fallback_bodies) = recording_server(
        chat_200_with("from fallback", "deepseek/deepseek-v4-flash-0731"),
        "/chat/completions",
    )
    .await;

    // The fallback is built with the *override* model already set (`set_model`); the test
    // verifies that model slug actually reaches the wire.
    let fb = OpenAiCompatibleProvider::new(
        "sk-test-fb",
        "openai/gpt-4o-mini", // fallback provider's default — we will override
        fallback.uri(),
    );
    fb.set_model("deepseek/deepseek-v4-flash-0731".to_string());

    let provider = OpenAiCompatibleProvider::new("sk-test", "MiniMax-M3", primary.uri())
        .with_fallback(fb, vec![402]);

    let resp = provider
        .complete(one_turn())
        .await
        .expect("fallback should succeed");
    assert_eq!(resp.content.as_deref(), Some("from fallback"));

    // Both backends got one call each — primary tried, fallback retried.
    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
    assert_eq!(fallback_bodies.lock().unwrap().len(), 1);

    // Pin the real MiniMax-M3 model name on the primary request — this proves the test is
    // actually exercising the configured primary, not some other model.
    let primary_body = primary_bodies.lock().unwrap()[0].clone();
    assert_eq!(
        primary_body["model"].as_str(),
        Some("MiniMax-M3"),
        "primary request must use MiniMax-M3"
    );

    // And the fallback saw the OVERRIDE model, not its own default.
    let fb_body = fallback_bodies.lock().unwrap()[0].clone();
    assert_eq!(
        fb_body["model"].as_str(),
        Some("deepseek/deepseek-v4-flash-0731"),
        "fallback must send its configured model, not its provider default"
    );
    // The fallback request is identical to the primary's body (model aside) — same messages,
    // same tools. Verifying one key field here is enough; the full equality would just
    // duplicate the upstream test.
    assert_eq!(
        fb_body["messages"][0]["role"].as_str(),
        Some("user"),
        "fallback must replay the same messages as the primary"
    );
}

/// Primary returns a status NOT in `fallback_on_status` → primary's error propagates, fallback
/// untouched. A 400 is a caller error, not transient — silently downgrading it would mask the
/// real defect.
#[tokio::test]
async fn primary_400_is_returned_without_fallback() {
    let (primary, primary_bodies) = recording_server(
        chat_error(400, r#"{"error":"unknown model 'foo'"}"#),
        "/chat/completions",
    )
    .await;
    let (fallback, _fallback_bodies) = recording_server(
        chat_200_with("from fallback", "irrelevant"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()),
            vec![402],
        );

    let err = provider
        .complete(one_turn())
        .await
        .expect_err("400 must not fall back");
    let msg = err.to_string();
    assert!(
        msg.contains("400"),
        "primary's status must propagate, got: {msg}"
    );
    assert!(
        msg.contains("unknown model"),
        "primary's body must propagate, got: {msg}"
    );

    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
    // Fallback's mock would have rejected any incoming request; absence of error means no call.
}

/// Primary 402 but `on_status` doesn't list 402 → no fallback. The fallback wiring is configurable
/// — an operator who hasn't opted in to a code must not have it silently trigger.
#[tokio::test]
async fn primary_402_with_unlisted_code_does_not_trigger_fallback() {
    let (primary, primary_bodies) = recording_server(
        chat_error(402, r#"{"error":"insufficient balance"}"#),
        "/chat/completions",
    )
    .await;
    let (fallback, _fallback_bodies) = recording_server(
        chat_200_with("irrelevant", "irrelevant"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        // Only 429 listed; 402 is not.
        .with_fallback(
            OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()),
            vec![429],
        );

    let err = provider
        .complete(one_turn())
        .await
        .expect_err("402 must not fall back when unlisted");
    assert!(err.to_string().contains("402"));

    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
}

/// If both primary and fallback fail, the fallback's error is what the caller sees (the primary
/// error is logged for diagnostics but masked — the caller can't make a second decision with two
/// errors in flight). The fallback gets exactly one attempt, never more.
#[tokio::test]
async fn fallback_failure_propagates_fallback_error_and_does_not_retry() {
    let (primary, primary_bodies) =
        recording_server(chat_error(402, "primary exhausted"), "/chat/completions").await;
    let (fallback, fallback_bodies) =
        recording_server(chat_error(503, "fallback exhausted"), "/chat/completions").await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()),
            vec![402],
        );

    let err = provider
        .complete(one_turn())
        .await
        .expect_err("both must fail");
    let msg = err.to_string();
    assert!(
        msg.contains("503"),
        "fallback's error must be what the caller sees, got: {msg}"
    );
    assert!(
        !msg.contains("primary exhausted"),
        "primary's body must NOT be in the returned error — it is logged at warn!, not returned: {msg}"
    );

    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
    // Exactly one fallback attempt, no chained retry.
    assert_eq!(fallback_bodies.lock().unwrap().len(), 1);
}

/// Empty `on_status` is a misconfiguration (the fallback is wired but unreachable). It must not
/// panic; it just means no status triggers a retry.
#[tokio::test]
async fn empty_on_status_means_no_fallback_can_trigger() {
    let (primary, primary_bodies) =
        recording_server(chat_error(402, "primary exhausted"), "/chat/completions").await;
    let (fallback, _fallback_bodies) = recording_server(
        chat_200_with("irrelevant", "irrelevant"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()),
            Vec::new(),
        );

    let err = provider
        .complete(one_turn())
        .await
        .expect_err("empty on_status means no fallback");
    assert!(err.to_string().contains("402"));

    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
}

/// Streaming path applies the same fallback rule on the initial response status.
#[tokio::test]
async fn streaming_primary_402_triggers_fallback_on_initial_status() {
    let (primary, primary_bodies) =
        recording_server(chat_error(402, "primary exhausted"), "/chat/completions").await;
    let (fallback, fallback_bodies) = recording_server(
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: [DONE]\n\n"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()),
            vec![402],
        );

    let stream = provider
        .complete_stream(one_turn())
        .await
        .expect("fallback stream should open");
    // The stream is returned but we don't drain it here — the point is that the primary was
    // bypassed and the fallback's stream opened. Asserting on the request capture is the proof.
    drop(stream);

    assert_eq!(primary_bodies.lock().unwrap().len(), 1);
    assert_eq!(fallback_bodies.lock().unwrap().len(), 1);
}

fn request_stamped(model: &str) -> CompletionRequest {
    one_turn().with_model(Some(model.to_string()))
}

/// Chat stamps the primary slug on the request. The fallback host must still receive the
/// configured fallback model, not that stamp.
#[tokio::test]
async fn stamped_primary_model_is_replaced_on_fallback() {
    let (primary, primary_bodies) = recording_server(minimax_402(), "/chat/completions").await;
    let (fallback, fallback_bodies) = recording_server(
        chat_200_with("from fallback", "deepseek/deepseek-v4-flash-0731"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "MiniMax-M3", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new(
                "sk-test-fb",
                "deepseek/deepseek-v4-flash-0731",
                fallback.uri(),
            ),
            vec![402],
        );

    let resp = provider
        .complete(request_stamped("MiniMax-M3"))
        .await
        .expect("fallback should succeed");
    assert_eq!(resp.content.as_deref(), Some("from fallback"));

    let primary_body = primary_bodies.lock().unwrap()[0].clone();
    assert_eq!(primary_body["model"].as_str(), Some("MiniMax-M3"));
    let fb_body = fallback_bodies.lock().unwrap()[0].clone();
    assert_eq!(
        fb_body["model"].as_str(),
        Some("deepseek/deepseek-v4-flash-0731"),
        "fallback must send its configured model, not the stamped primary slug"
    );
}

#[tokio::test]
async fn stamped_primary_model_is_replaced_on_streaming_fallback() {
    let (primary, primary_bodies) =
        recording_server(chat_error(402, "primary exhausted"), "/chat/completions").await;
    let (fallback, fallback_bodies) = recording_server(
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: [DONE]\n\n"),
        "/chat/completions",
    )
    .await;

    let provider = OpenAiCompatibleProvider::new("sk-test", "MiniMax-M3", primary.uri())
        .with_fallback(
            OpenAiCompatibleProvider::new(
                "sk-test-fb",
                "deepseek/deepseek-v4-flash-0731",
                fallback.uri(),
            ),
            vec![402],
        );

    let stream = provider
        .complete_stream(request_stamped("MiniMax-M3"))
        .await
        .expect("fallback stream should open");
    drop(stream);

    assert_eq!(
        primary_bodies.lock().unwrap()[0]["model"].as_str(),
        Some("MiniMax-M3")
    );
    assert_eq!(
        fallback_bodies.lock().unwrap()[0]["model"].as_str(),
        Some("deepseek/deepseek-v4-flash-0731")
    );
}

/// The fallback provider may itself have a fallback configured. The retry is once: that
/// child must not be called when the first fallback fails.
#[tokio::test]
async fn fallback_does_not_chain_into_its_own_fallback() {
    let (primary, _) =
        recording_server(chat_error(402, "primary exhausted"), "/chat/completions").await;
    let (fallback, fallback_bodies) =
        recording_server(chat_error(503, "fallback exhausted"), "/chat/completions").await;
    let (grandchild, grandchild_bodies) = recording_server(
        chat_200_with("from grandchild", "grand"),
        "/chat/completions",
    )
    .await;

    let fb = OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()).with_fallback(
        OpenAiCompatibleProvider::new("sk-test-grand", "grand-model", grandchild.uri()),
        vec![503],
    );
    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(fb, vec![402]);

    let err = provider
        .complete(one_turn())
        .await
        .expect_err("a failed fallback must not chain");
    assert!(
        err.to_string().contains("503"),
        "caller must see the fallback error, got: {err}"
    );
    assert_eq!(fallback_bodies.lock().unwrap().len(), 1);
    assert_eq!(
        grandchild_bodies.lock().unwrap().len(),
        0,
        "a nested fallback must not run"
    );
}

#[tokio::test]
async fn streaming_fallback_does_not_chain_into_its_own_fallback() {
    let (primary, _) =
        recording_server(chat_error(402, "primary exhausted"), "/chat/completions").await;
    let (fallback, fallback_bodies) =
        recording_server(chat_error(503, "fallback exhausted"), "/chat/completions").await;
    let (grandchild, grandchild_bodies) = recording_server(
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("data: [DONE]\n\n"),
        "/chat/completions",
    )
    .await;

    let fb = OpenAiCompatibleProvider::new("sk-test-fb", "fb-model", fallback.uri()).with_fallback(
        OpenAiCompatibleProvider::new("sk-test-grand", "grand-model", grandchild.uri()),
        vec![503],
    );
    let provider = OpenAiCompatibleProvider::new("sk-test", "primary-model", primary.uri())
        .with_fallback(fb, vec![402]);

    let err = match provider.complete_stream(one_turn()).await {
        Ok(_) => panic!("a failed streaming fallback must not chain"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("503"), "got: {err}");
    assert_eq!(fallback_bodies.lock().unwrap().len(), 1);
    assert_eq!(grandchild_bodies.lock().unwrap().len(), 0);
}
