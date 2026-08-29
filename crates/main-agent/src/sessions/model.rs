//! Per-conversation model stamp and derivation from the log.

use super::super::*;
use super::test_fixtures::*;

// ── Per-conversation model: recorded on the log, derived back from it ────────

/// The stamp lands on the turn's nodes, so the log says which model answered.
#[tokio::test]
async fn a_turn_records_the_model_it_ran_on() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("hi")]).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    let nodes = sessions.store.leaf_path(id, None).await.unwrap();
    let stamped: Vec<_> = nodes
        .iter()
        .filter_map(|n| n.model.as_deref().map(|m| (n.author.clone(), m)))
        .collect();
    assert_eq!(
        stamped,
        vec![(Author::User, "mock"), (Author::Assistant, "mock")],
        "both the question's model and the answer's should be on the log, and nothing else's"
    );
    // The system prompt is nobody's model.
    assert!(nodes[0].model.is_none());
}

/// The point of recording it: the next turn goes to the same model without anything storing a
/// "selected model" field that could disagree with what ran.
#[tokio::test]
async fn the_next_turn_follows_the_model_already_on_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(None).await.unwrap();

    sessions.select_model(id, "chosen-model".into());
    assert_eq!(
        sessions.turn_settings(id).await.model.as_deref(),
        Some("chosen-model"),
        "the pending pick must win for the turn that follows it"
    );

    // Consumed: a second read has nothing pending and falls through to the log, which is still
    // empty of model stamps, so it lands on the provider default.
    assert_eq!(sessions.turn_settings(id).await.model, None);

    // Run a turn, and the log becomes the source.
    sessions.turn(id, "hello").await.unwrap();
    assert_eq!(
        sessions.turn_settings(id).await.model.as_deref(),
        Some("mock"),
        "with history, the conversation stays on whatever last answered it"
    );
}

/// A tool result is produced by an MCP, not a model. Stamping it would make the derivation report a
/// model for a turn no model spoke in.
#[tokio::test]
async fn tool_results_carry_no_model() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("done")]).await;
    let id = sessions.create(None).await.unwrap();

    // Append a tool node directly — the shape a tool-calling turn leaves behind.
    let parent = sessions
        .store
        .leaf_path(id, None)
        .await
        .unwrap()
        .last()
        .map(|n| n.id);
    sessions
        .store
        .append(
            id,
            NewNode {
                parent_id: parent,
                author: Author::Tool,
                message: Message::tool_result("call-1", "result"),
                model: None,
            },
        )
        .await
        .unwrap();

    assert_eq!(
        sessions.turn_settings(id).await.model,
        None,
        "a tool node must not be read back as the conversation's model"
    );
}

/// Derivation keys on `Author`, not `message.role`. A subagent handoff is authored `goal-session`
/// with an assistant-role body; reading by role would migrate the conversation onto whatever model
/// a delegation happened to use.
#[tokio::test]
async fn a_subagent_handoff_does_not_capture_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), vec![CompletionResponse::text("ok")]).await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "hello").await.unwrap();

    // A goal-session note lands after the turn, carrying an assistant-role body.
    sessions
        .append_note(id, "the specialist finished")
        .await
        .unwrap();

    assert_eq!(
        sessions.turn_settings(id).await.model.as_deref(),
        Some("mock"),
        "the last *assistant-authored* model still decides, not the note that followed it"
    );
}

/// A conversation whose log predates this field has no stamp anywhere, and must fall back to the
/// provider default rather than failing or inventing one.
#[tokio::test]
async fn a_conversation_with_no_stamps_falls_back_to_the_provider() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = sessions_at(dir.path(), Vec::new()).await;
    let id = sessions.create(None).await.unwrap();
    assert_eq!(sessions.turn_settings(id).await.model, None);
}
