//! Integration coverage for `liberado-provider`: dyn object-safety (the daemon holds
//! `dyn Provider`), the `complete_json` error contract (Decision 13 resilience), builder
//! correctness, and serde round-trips of the wire types.

use liberado_provider::*;

#[tokio::test]
async fn provider_is_object_safe_behind_dyn() {
    // The whole point of the trait shape: the daemon stores `dyn Provider` and swaps models.
    let provider: Box<dyn Provider> = Box::new(MockProvider::with_script(
        "m",
        [CompletionResponse::text("hello")],
    ));
    assert_eq!(provider.model(), "m");
    let resp = provider
        .complete(CompletionRequest::new(vec![Message::user("hi")]))
        .await
        .unwrap();
    assert_eq!(resp.content.as_deref(), Some("hello"));

    // ...and `complete_json` accepts `?Sized`, so it works through the trait object too.
    let mock = MockProvider::with_script("m", [CompletionResponse::text(r#"{"ok":true}"#)]);
    let by_ref: &dyn Provider = &mock;
    let v: serde_json::Value = complete_json(
        by_ref,
        CompletionRequest::new(vec![Message::user("q")]),
        serde_json::json!({ "type": "object" }),
    )
    .await
    .unwrap();
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn complete_json_maps_empty_content_to_empty_response() {
    // A pure tool-call turn has no text content.
    let mock = MockProvider::with_script(
        "m",
        [CompletionResponse::tool_calls(vec![ToolInvocation::new(
            "1",
            "t",
            serde_json::json!({}),
        )])],
    );
    let res: ProviderResult<serde_json::Value> =
        complete_json(&mock, CompletionRequest::new(vec![]), serde_json::json!({})).await;
    assert!(matches!(res, Err(ProviderError::EmptyResponse)));
}

#[tokio::test]
async fn complete_json_maps_bad_json_to_decode_error() {
    let mock = MockProvider::with_script("m", [CompletionResponse::text("definitely not json")]);
    let res: ProviderResult<serde_json::Value> =
        complete_json(&mock, CompletionRequest::new(vec![]), serde_json::json!({})).await;
    assert!(matches!(res, Err(ProviderError::Decode(_))));
}

#[tokio::test]
async fn complete_json_propagates_provider_error() {
    let mock = MockProvider::new("m"); // no scripted responses
    let res: ProviderResult<serde_json::Value> =
        complete_json(&mock, CompletionRequest::new(vec![]), serde_json::json!({})).await;
    assert!(matches!(res, Err(ProviderError::MockExhausted)));
}

#[test]
fn request_builders_set_all_fields() {
    let req = CompletionRequest::new(vec![Message::system("s"), Message::user("u")])
        .with_tools(vec![ToolDef::new(
            "search",
            "search the vault",
            serde_json::json!({ "type": "object" }),
        )])
        .with_temperature(0.0)
        .with_max_tokens(256)
        .with_json_schema(serde_json::json!({ "type": "object" }));

    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.temperature, Some(0.0));
    assert_eq!(req.max_tokens, Some(256));
    assert!(matches!(req.response_format, ResponseFormat::Json { .. }));
}

#[test]
fn message_constructors_set_role_and_linkage() {
    assert_eq!(Message::system("x").role, Role::System);
    assert_eq!(Message::user("x").role, Role::User);
    assert_eq!(Message::assistant("x").role, Role::Assistant);

    let t = Message::tool_result("call-42", "result body");
    assert_eq!(t.role, Role::Tool);
    assert_eq!(t.tool_call_id.as_deref(), Some("call-42"));
    assert!(t.tool_calls.is_empty());
}

#[test]
fn response_helpers_set_finish_reason() {
    assert_eq!(
        CompletionResponse::text("hi").finish_reason,
        FinishReason::Stop
    );
    let tc =
        CompletionResponse::tool_calls(vec![ToolInvocation::new("1", "t", serde_json::json!({}))]);
    assert_eq!(tc.finish_reason, FinishReason::ToolCalls);
    assert!(tc.content.is_none());
}

#[test]
fn request_and_response_serde_round_trip() {
    let req = CompletionRequest::new(vec![Message::user("hi")])
        .with_json_schema(serde_json::json!({ "type": "object" }));
    let back: CompletionRequest =
        serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
    assert_eq!(req, back);

    let resp = CompletionResponse {
        content: Some("c".into()),
        tool_calls: vec![ToolInvocation::new("1", "t", serde_json::json!({ "a": 1 }))],
        finish_reason: FinishReason::ToolCalls,
        usage: Some(Usage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
            cached_prompt_tokens: None,
        }),
    };
    let back: CompletionResponse =
        serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
    assert_eq!(resp, back);
}
