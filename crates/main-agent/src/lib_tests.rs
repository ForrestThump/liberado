//! Survivor tests for `Conversation` primitives that the inline module did not pin.

use super::*;
use async_trait::async_trait;
use liberado_executor::Budget;
use liberado_provider::{
    CompletionRequest, CompletionResponse, MockProvider, Provider, ProviderResult, ToolDef,
    ToolInvocation,
};
use std::sync::Arc;
use tokio::sync::mpsc;

struct NoTools;
#[async_trait]
impl ToolRuntime for NoTools {
    fn catalog(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    async fn invoke(&self, _call: &ToolInvocation) -> Result<String, String> {
        Err("no tools".into())
    }
}

/// `is_empty` must agree with the underlying buffer in both directions.
#[test]
fn is_empty_tracks_the_history() {
    assert!(Conversation::from_history(Vec::new()).is_empty());
    let mut convo = Conversation::new("sys");
    assert!(
        !convo.is_empty(),
        "a fresh conversation holds the system prompt"
    );
    convo.answer("u", "a");
    assert!(!convo.is_empty());
}

/// `resume_stream` appends the human's answer as a **tool result** and drives the loop — a
/// short-circuit `Ok(())` would leave the model forever awaiting a tool it already got.
#[tokio::test]
async fn resume_stream_appends_the_tool_result_and_completes() {
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("all done")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut convo = Conversation::new("sys");
    let (tx, _rx) = mpsc::channel(8);

    convo
        .resume_stream(&executor, &NoTools, "call-7", "42 degrees", &tx)
        .await
        .unwrap();

    // The answer reached the provider as a tool result keyed by call id…
    let sent = &provider.received_requests()[0];
    let result = sent
        .messages
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("the human's answer rides as a tool result");
    assert_eq!(result.tool_call_id.as_deref(), Some("call-7"));
    assert_eq!(result.content, "42 degrees");
    // …and the model spoke again.
    let last = convo.history().last().unwrap();
    assert_eq!(last.role, Role::Assistant);
    assert_eq!(last.content, "all done");
}

/// The rollback is what makes an aborted streaming turn leave no trace. The old test never
/// polled the future, so neither arm of the drop ever ran; this one polls once (past the user
/// push and the arm), then drops the future mid-body.
#[tokio::test]
async fn aborting_a_polled_stream_turn_rolls_back_to_clean_history() {
    struct PendingProvider;
    #[async_trait]
    impl Provider for PendingProvider {
        fn model(&self) -> String {
            "pending".into()
        }
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> ProviderResult<CompletionResponse> {
            std::future::pending().await
        }
    }

    let executor = Executor::new(Arc::new(PendingProvider), Budget::default());
    let mut convo = Conversation::new("sys");
    let (tx, _rx) = mpsc::channel(1);

    let mut fut = Box::pin(convo.turn_stream(&executor, &NoTools, "hi", &tx));
    // One poll runs the body up to its first await: user message pushed, rollback armed,
    // provider parked forever. A no-op waker is fine — nothing here relies on waking.
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    assert!(
        matches!(fut.as_mut().poll(&mut cx), std::task::Poll::Pending),
        "the turn parks on the pending provider"
    );
    drop(fut); // cancellation → Rollback fires

    assert_eq!(
        convo.history().len(),
        1,
        "only the system prompt may survive an aborted turn"
    );
    assert_eq!(convo.history()[0].role, Role::System);
}

/// Transient system injections are skipped when slicing what a turn added. A decremented
/// counter underflows (debug panic) or over-slices — both fail here.
#[test]
fn turn_tail_skips_transient_injections() {
    let mut convo = Conversation::new("sys");
    convo.answer("u1", "a1");
    let before = convo.len(); // [sys, u1, a1]

    let tools = vec![ToolDef::new("t", "d", serde_json::json!({}))];
    convo.apply_available_tools(&tools); // transient inserted at index 1

    convo.answer("u2", "a2"); // the turn's own exchange

    let tail = convo.turn_tail(before);
    let rendered: Vec<&str> = tail.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(
        rendered,
        ["u2", "a2"],
        "transient manifest must not leak into the tail"
    );
}
