use super::*;

#[tokio::test]
async fn converse_messages_reserves_a_tool_free_final_response() {
    let (provider, exec) = executor(
        vec![
            call_tool("search"),
            CompletionResponse::text("Search succeeded; here is the result."),
        ],
        Budget::new(1),
    );
    let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));
    let mut messages = vec![Message::system("helper"), Message::user("find")];

    let answer = exec
        .converse_messages(&runtime, &mut messages)
        .await
        .unwrap();

    assert_eq!(answer, "Search succeeded; here is the result.");
    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].tools.is_empty(),
        "the reserve must not permit more work"
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|m| m.content.contains("All tools are now withdrawn"))
    );
}
#[tokio::test]
async fn converse_stream_reserves_a_tool_free_final_response() {
    let (provider, exec) = executor(
        vec![
            call_tool("search"),
            CompletionResponse::text("Search succeeded; here is the result."),
        ],
        Budget::new(1),
    );
    let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let mut messages = vec![Message::system("helper"), Message::user("find")];

    exec.converse_stream(&runtime, &mut messages, &tx)
        .await
        .unwrap();

    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.is_empty());
}
