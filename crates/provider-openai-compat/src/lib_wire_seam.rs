//! Split from `lib.rs` for module-health boundaries.

use super::*;
use liberado_provider::{Message, ResponseFormat, ToolDef};
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Captures each request body it serves, so a test can assert on what was sent.
pub(crate) struct Capture {
    bodies: Arc<Mutex<Vec<Value>>>,
    response: ResponseTemplate,
}

impl Respond for Capture {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body)
            .expect("outgoing request body should be valid JSON");
        self.bodies.lock().unwrap().push(body);
        self.response.clone()
    }
}

/// A chat-completions server that records what it was sent. Returns the server (which must be
/// kept alive for the duration of the call) and the handle the bodies land in.
pub(crate) async fn recording_server(
    response: ResponseTemplate,
) -> (MockServer, Arc<Mutex<Vec<Value>>>) {
    let server = MockServer::start().await;
    let bodies = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Capture {
            bodies: Arc::clone(&bodies),
            response,
        })
        .mount(&server)
        .await;
    (server, bodies)
}

pub(crate) fn ok_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{ "message": { "role": "assistant", "content": "ok" } }]
    }))
}

fn sse_reply() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string("data: [DONE]\n\n")
}

fn provider_at(server: &MockServer) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new("sk-test", "test-model", server.uri())
}

/// Drive one blocking call through `provider` and hand back the body it sent.
async fn sent_by_complete(
    build: impl FnOnce(OpenAiCompatibleProvider) -> OpenAiCompatibleProvider,
    request: CompletionRequest,
) -> Value {
    let (server, bodies) = recording_server(ok_reply()).await;
    let provider = build(provider_at(&server));
    provider.complete(request).await.expect("call failed");
    let sent = bodies.lock().unwrap();
    sent.first().cloned().expect("no request was captured")
}

/// Same, for the streaming path.
async fn sent_by_stream(
    build: impl FnOnce(OpenAiCompatibleProvider) -> OpenAiCompatibleProvider,
    request: CompletionRequest,
) -> Value {
    let (server, bodies) = recording_server(sse_reply()).await;
    let provider = build(provider_at(&server));
    let _stream = provider
        .complete_stream(request)
        .await
        .expect("stream call failed");
    let sent = bodies.lock().unwrap();
    sent.first().cloned().expect("no request was captured")
}

fn one_turn() -> CompletionRequest {
    CompletionRequest::new(vec![Message::user("hi")])
}

/// Temperatures are `f32` in the request type and `f64` once serialized, so `0.7` arrives as
/// `0.699999988079071`. Compare the value, not the representation.
fn assert_temperature(body: &Value, expected: f32) {
    let actual = body["temperature"]
        .as_f64()
        .unwrap_or_else(|| panic!("no temperature in {body}"));
    assert!(
        (actual - expected as f64).abs() < 1e-6,
        "expected temperature ~{expected}, got {actual}"
    );
}

// ---- the model id ----

#[tokio::test]
async fn the_active_model_is_what_gets_sent() {
    let (server, bodies) = recording_server(ok_reply()).await;
    let provider = provider_at(&server);
    provider.set_model("deepseek/deepseek-v4-flash".into());
    provider.complete(one_turn()).await.unwrap();
    assert_eq!(
        bodies.lock().unwrap()[0]["model"],
        json!("deepseek/deepseek-v4-flash"),
        "a hot-swapped model must reach the wire, not just the RwLock"
    );
}

// ---- role temperature ----

#[tokio::test]
async fn role_temperature_overrides_the_per_request_value() {
    // The field doc promises an override, not a default. A role pinned to 0 exists precisely
    // to beat a caller that asked for something warmer.
    let mut request = one_turn();
    request.temperature = Some(0.7);
    let body = sent_by_complete(|p| p.with_temperature(Some(0.0)), request).await;
    assert_temperature(&body, 0.0);
}

#[tokio::test]
async fn without_a_role_temperature_the_request_value_survives() {
    let mut request = one_turn();
    request.temperature = Some(0.7);
    let body = sent_by_complete(|p| p, request).await;
    assert_temperature(&body, 0.7);
}

#[tokio::test]
async fn no_temperature_anywhere_sends_no_temperature_field() {
    // Distinct from sending `null` — some backends reject an explicit null where they would
    // happily apply their own default for an absent key.
    let body = sent_by_complete(|p| p, one_turn()).await;
    assert!(
        body.get("temperature").is_none(),
        "expected the key to be absent, got {:?}",
        body.get("temperature")
    );
}

// ---- reasoning effort ----

#[tokio::test]
async fn reasoning_off_disables_thinking_on_the_wire() {
    let body = sent_by_complete(|p| p.with_reasoning_effort(Some("off".into())), one_turn()).await;
    assert_eq!(body["reasoning"], json!({ "enabled": false }));
}

#[tokio::test]
async fn the_other_disable_spellings_mean_the_same_thing() {
    for spelling in ["none", "disabled"] {
        let body = sent_by_complete(
            |p| p.with_reasoning_effort(Some(spelling.into())),
            one_turn(),
        )
        .await;
        assert_eq!(
            body["reasoning"],
            json!({ "enabled": false }),
            "{spelling} should disable reasoning like `off` does"
        );
    }
}

#[tokio::test]
async fn a_reasoning_level_is_sent_as_an_effort() {
    for level in ["low", "medium", "high"] {
        let body =
            sent_by_complete(|p| p.with_reasoning_effort(Some(level.into())), one_turn()).await;
        assert_eq!(body["reasoning"], json!({ "effort": level }));
    }
}

#[tokio::test]
async fn no_reasoning_override_sends_no_reasoning_field() {
    let body = sent_by_complete(|p| p, one_turn()).await;
    assert!(
        body.get("reasoning").is_none(),
        "an unset role reasoning level must leave the provider/model default alone"
    );
}

#[tokio::test]
async fn request_reasoning_is_sent_when_the_provider_has_none() {
    let mut request = one_turn();
    request.reasoning = Some("high".into());
    let body = sent_by_complete(|p| p, request).await;
    assert_eq!(body["reasoning"], json!({ "effort": "high" }));
}

// ---- tools ----

#[tokio::test]
async fn tools_reach_the_wire_with_their_schemas_intact() {
    let mut request = one_turn();
    request.tools = vec![ToolDef {
        name: "vault_search".into(),
        description: "search the vault".into(),
        parameters: json!({
            "type": "object",
            "properties": { "query": { "type": "string" } },
            "required": ["query"],
        }),
    }];
    let body = sent_by_complete(|p| p, request).await;
    let tools = body["tools"].as_array().expect("tools should be an array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["function"]["name"], json!("vault_search"));
    // The parameter schema is the part a model actually needs; dropping it is silent and fatal.
    assert_eq!(
        tools[0]["function"]["parameters"]["required"],
        json!(["query"]),
        "a tool's parameter schema must survive the trip"
    );
}

#[tokio::test]
async fn no_tools_sends_no_tools_field() {
    // An empty `tools: []` is not the same request as one with no tools — some backends
    // error on it, and it changes how others prompt.
    let body = sent_by_complete(|p| p, one_turn()).await;
    assert!(
        body.get("tools").is_none(),
        "expected no tools key, got {:?}",
        body.get("tools")
    );
}

// ---- response format ----

#[tokio::test]
async fn a_shape_constraining_schema_is_sent_as_strict_json_schema() {
    // The regression this whole module exists for: four callers built this schema correctly
    // and the request builder threw it away.
    let schema = json!({
        "type": "object",
        "properties": { "kind": { "type": "string" } },
        "required": ["kind"],
    });
    let mut request = one_turn();
    request.response_format = ResponseFormat::Json {
        schema: schema.clone(),
    };
    let body = sent_by_complete(|p| p, request).await;
    assert_eq!(body["response_format"]["type"], json!("json_schema"));
    assert_eq!(
        body["response_format"]["json_schema"]["strict"],
        json!(true)
    );
    assert_eq!(
        body["response_format"]["json_schema"]["schema"], schema,
        "the caller's schema must be the one sent"
    );
}

#[tokio::test]
async fn a_shapeless_schema_falls_back_to_plain_json_mode() {
    // `strict` mode cannot express "an object, contents unspecified", so a schema that
    // constrains nothing must degrade to json_object rather than be sent and rejected.
    let mut request = one_turn();
    request.response_format = ResponseFormat::Json {
        schema: json!({ "type": "object" }),
    };
    let body = sent_by_complete(|p| p, request).await;
    assert_eq!(body["response_format"], json!({ "type": "json_object" }));
}

// ---- max_tokens ----

#[tokio::test]
async fn max_tokens_reaches_the_wire() {
    let mut request = one_turn();
    request.max_tokens = Some(512);
    let body = sent_by_complete(|p| p, request).await;
    assert_eq!(body["max_tokens"], json!(512));
}

#[tokio::test]
async fn no_max_tokens_sends_no_max_tokens_field() {
    let body = sent_by_complete(|p| p, one_turn()).await;
    assert!(
        body.get("max_tokens").is_none(),
        "an absent cap must not become an explicit one"
    );
}

#[tokio::test]
async fn plain_text_requests_send_no_response_format() {
    let body = sent_by_complete(|p| p, one_turn()).await;
    assert!(
        body.get("response_format").is_none(),
        "a plain completion must not be silently constrained to JSON"
    );
}

// ---- the streaming path carries everything the blocking path does ----

#[tokio::test]
async fn streaming_sends_the_same_role_tuning_as_blocking() {
    let build = |p: OpenAiCompatibleProvider| {
        p.with_temperature(Some(0.25))
            .with_reasoning_effort(Some("high".into()))
    };
    let blocking = sent_by_complete(build, one_turn()).await;
    let streaming = sent_by_stream(build, one_turn()).await;

    assert_eq!(streaming["temperature"], blocking["temperature"]);
    assert_eq!(streaming["reasoning"], blocking["reasoning"]);
    assert_temperature(&streaming, 0.25);
    assert_eq!(streaming["reasoning"], json!({ "effort": "high" }));
}

#[tokio::test]
async fn streaming_asks_for_the_trailing_usage_chunk() {
    // Without this the latency journal records zero tokens for every streamed call, which is
    // the kind of quiet wrongness that reads as a real measurement.
    let body = sent_by_stream(|p| p, one_turn()).await;
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["stream_options"], json!({ "include_usage": true }));
}

#[tokio::test]
async fn the_blocking_path_does_not_ask_to_stream() {
    let body = sent_by_complete(|p| p, one_turn()).await;
    assert!(
        body.get("stream").is_none() || body["stream"] == json!(false),
        "blocking calls must not set stream=true"
    );
    assert!(body.get("stream_options").is_none());
}

// ---- auth ----

#[tokio::test]
async fn the_api_key_is_sent_as_a_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer sk-test",
        ))
        .respond_with(ok_reply())
        .expect(1)
        .mount(&server)
        .await;
    provider_at(&server).complete(one_turn()).await.unwrap();
    // `expect(1)` is verified on drop — an unauthenticated request would not have matched.
}
