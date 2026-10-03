//! MiniMax keeps its reasoning effort. The user-visible answer does not include it.

use futures::StreamExt;
use liberado_provider::{CompletionRequest, Message, Provider, StreamItem};
use serde_json::json;
use wiremock::ResponseTemplate;

use super::OpenAiCompatibleProvider;
use super::reasoning_channel::targets_minimax;
use super::wire_seam::{ok_reply, recording_server};

fn one_turn() -> CompletionRequest {
    CompletionRequest::new(vec![Message::user("hi")])
}

fn sse_done() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string("data: [DONE]\n\n")
}

async fn sent(model: &str, effort: &str, stream: bool) -> serde_json::Value {
    let reply = if stream { sse_done() } else { ok_reply() };
    let (server, bodies) = recording_server(reply).await;
    let provider = OpenAiCompatibleProvider::new("sk-test", model, server.uri())
        .with_reasoning_effort(Some(effort.into()));
    if stream {
        let _stream = provider
            .complete_stream(one_turn())
            .await
            .expect("stream open");
    } else {
        provider.complete(one_turn()).await.expect("complete");
    }
    bodies.lock().unwrap()[0].clone()
}

#[test]
fn minimax_is_detected_from_the_model_or_the_base_url() {
    assert!(targets_minimax("MiniMax-M3", "http://127.0.0.1:9"));
    assert!(targets_minimax("minimax/minimax-m2", "http://example.test"));
    assert!(targets_minimax(
        "deepseek-chat",
        "https://api.minimax.io/v1"
    ));
    assert!(!targets_minimax(
        "deepseek-chat",
        "https://api.deepseek.com"
    ));
    assert!(!targets_minimax("test-model", "http://127.0.0.1:9"));
}

#[tokio::test]
async fn minimax_requests_keep_high_effort_and_split_the_channel() {
    for stream in [false, true] {
        let body = sent("MiniMax-M3", "high", stream).await;
        assert_eq!(
            body["reasoning"],
            json!({ "effort": "high" }),
            "stream={stream}"
        );
        assert_eq!(body["reasoning_split"], json!(true), "stream={stream}");
    }
}

#[tokio::test]
async fn minimax_off_still_splits_and_does_not_raise_the_effort() {
    let body = sent("MiniMax-M3", "off", false).await;
    assert_eq!(body["reasoning"], json!({ "enabled": false }));
    assert_eq!(body["reasoning_split"], json!(true));
}

#[tokio::test]
async fn other_backends_do_not_receive_reasoning_split() {
    let blocking = sent("test-model", "high", false).await;
    assert_eq!(blocking["reasoning"], json!({ "effort": "high" }));
    assert!(blocking.get("reasoning_split").is_none());
    let streaming = sent("test-model", "high", true).await;
    assert!(streaming.get("reasoning_split").is_none());
}

#[tokio::test]
async fn a_blocking_answer_drops_think_tags_and_ignores_reasoning_content() {
    let reply = ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "<think>\nhidden\n</think>\n\nVisible",
                "reasoning_content": "hidden",
                "reasoning_details": [{ "text": "hidden" }]
            },
            "finish_reason": "stop"
        }]
    }));
    let (server, _) = recording_server(reply).await;
    let provider = OpenAiCompatibleProvider::new("sk-test", "MiniMax-M3", server.uri())
        .with_reasoning_effort(Some("high".into()));
    let resp = provider.complete(one_turn()).await.unwrap();
    assert_eq!(resp.content.as_deref(), Some("Visible"));
}

#[tokio::test]
async fn a_stream_hides_a_split_think_tag_and_the_reasoning_channel() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"nk>\\nsecret\\n</think>\\n\\nHello\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"secret\"}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let reply = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse);
    let (server, _) = recording_server(reply).await;
    let provider = OpenAiCompatibleProvider::new("sk-test", "MiniMax-M3", server.uri())
        .with_reasoning_effort(Some("high".into()));
    let mut stream = provider.complete_stream(one_turn()).await.unwrap();
    let mut tokens = String::new();
    let mut done = None;
    while let Some(item) = stream.next().await {
        match item.expect("stream item") {
            StreamItem::Token(text) => {
                assert!(
                    !text.to_ascii_lowercase().contains("<think") && !text.contains("secret"),
                    "thinking leaked into a token: {text:?}"
                );
                tokens.push_str(&text);
            }
            StreamItem::Done(resp) => done = Some(resp),
        }
    }
    assert_eq!(tokens, "Hello");
    assert_eq!(
        done.expect("stream finished").content.as_deref(),
        Some("Hello")
    );
}
