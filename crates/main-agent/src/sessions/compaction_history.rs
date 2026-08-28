//! Compaction tail copies vs rendered history.

use super::super::*;
use super::test_fixtures::*;

/// Compaction re-appends the kept tail so the model view is a contiguous log suffix. Those copies
/// must not surface to readers that walk the raw leaf path, or every compaction repeats the last
/// `keep_recent_turns` turns in rendered history and shifts `Author::User` turn indices (fork /
/// rewind resolves "turn N" against that count).
#[tokio::test]
async fn compaction_tail_copies_are_not_visible_in_rendered_history() {
    let dir = tempfile::tempdir().unwrap();
    let summary = "SUMMARY: rolled up".to_string();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1, // always fire
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("fresh answer"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(&sessions, id, &[("u-one", "a-one"), ("TAILMARK", "a-tail")]).await;

    let user_turns_before = sessions
        .history(id)
        .await
        .unwrap()
        .iter()
        .filter(|m| m.role == Role::User)
        .count();

    sessions.turn(id, "fresh question").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert_eq!(
        history.iter().filter(|m| m.content == "TAILMARK").count(),
        1,
        "the kept tail must appear once in rendered history, not once per compaction"
    );
    // Compaction never deletes: the elided originals and the marker are both still rendered.
    assert!(
        history.iter().any(|m| m.content == "u-one"),
        "elided originals must still render in full history"
    );
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "the marker must render as a checkpoint bubble"
    );
    // Turn indexing (fork/rewind counts `Author::User` nodes) gained exactly the new turn.
    let user_turns_after = history.iter().filter(|m| m.role == Role::User).count();
    assert_eq!(
        user_turns_after,
        user_turns_before + 1,
        "compaction must not inflate the user-turn count that fork/rewind indexes against"
    );
}

// ── D3: compaction tail copy audit ──────────────────────────────────────────
//
// Compaction writes `[marker] → [verbatim tail copies]` so the model sees a
// contiguous suffix. The originals are still on the log before the marker;
// every human-facing reader must skip `Author::is_compaction_tail_copy()` or
// it double-counts the kept tail. These tests verify that contract.

/// After compaction, `history_nodes()` returns original nodes + the marker only —
/// the re-appended tail copies are NOT in the rendered transcript.
#[tokio::test]
async fn history_nodes_excludes_tail_copies_after_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1, // always fire
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("summary"),
            CompletionResponse::text("fresh reply"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 secret alpha", "a1"),
            ("u2 secret beta", "a2"),
            // keep_recent_turns=1 → tail = ("tail q", "tail a")
            ("tail q", "tail a"),
        ],
    )
    .await;

    // Count before compaction: 1 system + 3 user/assistant pairs = 7 nodes
    let before = sessions.history_nodes(id).await.unwrap();
    assert_eq!(
        before.len(),
        7,
        "before compaction: 1 system + 6 turn nodes"
    );

    sessions.turn(id, "fresh q").await.unwrap();

    let after = sessions.history_nodes(id).await.unwrap();
    // After: 7 originals + 1 marker + 1 user + 1 assistant (fresh turn) = 10
    // Tail copies (2 nodes) must be filtered out
    assert_eq!(
        after.len(),
        10,
        "after compaction: originals + marker + fresh turn, no tail copies"
    );
    let tail_copy_count = after
        .iter()
        .filter(|n| n.author.is_compaction_tail_copy())
        .count();
    assert_eq!(tail_copy_count, 0, "zero tail copies in history_nodes");
}

/// `history()` preserves the full original transcript — elided content and the
/// marker are present, but tail copies (which duplicate the kept tail) are not.
#[tokio::test]
async fn history_preserves_originals_excludes_tail_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let summary = "ROLLING: secrets about ancient texts".to_string();

    // Sized so the seeded history far exceeds the trigger, while the
    // post-compaction view stays manageable.
    let trigger = compaction::estimate_tokens(&[
        Message::system(DEFAULT_SYSTEM_PROMPT),
        compaction::marker_message(&summary),
        Message::user("tail q"),
        Message::assistant("tail a"),
        Message::user("fresh q"),
    ]);
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: trigger,
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("fresh reply"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    let secret = format!("ELIDED-SECRET-{}", "x".repeat(600));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret, &format!("A1-{}", "y".repeat(600))),
            (
                &format!("U2-{}", "z".repeat(600)),
                &format!("A2-{}", "w".repeat(600)),
            ),
            ("tail q", "tail a"),
        ],
    )
    .await;

    sessions.turn(id, "fresh q").await.unwrap();

    let history = sessions.history(id).await.unwrap();

    // Original elided content must still be present
    assert!(
        history.iter().any(|m| m.content.contains("ELIDED-SECRET")),
        "elided content preserved in full history"
    );
    // The marker is present once
    let marker_count = history
        .iter()
        .filter(|m| m.content.starts_with(compaction::SUMMARY_HEADER))
        .count();
    assert_eq!(marker_count, 1, "one marker in history");
    // Tail content appears exactly once (the original before the marker, not the copy)
    let tail_q_count = history.iter().filter(|m| m.content == "tail q").count();
    assert_eq!(
        tail_q_count, 1,
        "tail question appears once, not duplicated"
    );
    let tail_a_count = history.iter().filter(|m| m.content == "tail a").count();
    assert_eq!(tail_a_count, 1, "tail answer appears once, not duplicated");
}

/// After two rolling compactions, `history()` shows both markers and every
/// original message exactly once — no tail-copy duplication from either fire.
#[tokio::test]
async fn history_after_rolling_compactions_no_duplicate_content() {
    let dir = tempfile::tempdir().unwrap();
    let summary_a = "SUMMARY-A".to_string();
    let summary_b = "SUMMARY-B".to_string();

    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1, // always fire
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary_a),
            CompletionResponse::text("reply after first compact"),
            CompletionResponse::text(summary_b),
            CompletionResponse::text("reply after second compact"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u-secret-alpha", "a-alpha"),
            ("u-mid", "a-mid"),
            ("u-tail-1", "a-tail-1"),
        ],
    )
    .await;

    // Compaction 1 fires
    sessions.turn(id, "q1").await.unwrap();

    // Grow past the post-compact suffix
    seed_turns(
        &sessions,
        id,
        &[("u-secret-beta", "a-beta"), ("u-tail-2", "a-tail-2")],
    )
    .await;

    // Compaction 2 fires
    sessions.turn(id, "q2").await.unwrap();

    let history = sessions.history(id).await.unwrap();

    // Both markers present
    let marker_count = history
        .iter()
        .filter(|m| m.content.starts_with(compaction::SUMMARY_HEADER))
        .count();
    assert_eq!(marker_count, 2, "two markers after two compactions");

    // Every unique message appears exactly once
    for content in &[
        "u-secret-alpha",
        "u-mid",
        "u-tail-1",
        "u-secret-beta",
        "u-tail-2",
    ] {
        let count = history.iter().filter(|m| m.content == *content).count();
        assert_eq!(
            count, 1,
            "{content} appears exactly once (not duplicated by tail copies from either compaction)"
        );
    }
}

/// `model_last_used()` derives the model from the log by scanning
/// `Author::User | Author::Assistant` nodes in reverse. Tail copies are
/// `Author::Named`, so they must be invisible to this scan. The absence of
/// `is_compaction_tail_copy()` here is correct by design, not an oversight.
#[tokio::test]
async fn model_last_used_ignores_tail_copies() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1,
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("summary"),
            CompletionResponse::text("fresh reply"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 hello", "a1 world"),
            ("u2 foo", "a2 bar"),
            ("tail Q", "tail A"),
        ],
    )
    .await;

    // Before compaction — seed_turns doesn't stamp model, so model_last_used is None
    assert!(sessions.model_last_used(id).await.is_none());

    // Turn triggers compaction, which appends tail copies (Author::Named).
    // The fresh assistant reply IS stamped with the provider's model ("mock").
    // model_last_used must report "mock", not be fooled by unstamped tail copies
    // that sit between the last real assistant node and the fresh reply.
    sessions.turn(id, "trigger compaction").await.unwrap();

    let model = sessions.model_last_used(id).await;
    assert_eq!(
        model.as_deref(),
        Some("mock"),
        "model_last_used finds the stamped assistant reply after compaction, ignoring tail copies"
    );
}

/// The raw leaf path from the store contains tail copies after compaction;
/// `history_nodes()` filters them out. This is the on-disk vs in-memory
/// contract: the store records what happened, the kernel presents it once.
#[tokio::test]
async fn raw_store_has_tail_copies_but_history_nodes_filters_them() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1,
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("summary"),
            CompletionResponse::text("fresh reply"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 hello", "a1 world"),
            ("u2 foo", "a2 bar"),
            ("tail Q", "tail A"),
        ],
    )
    .await;

    sessions.turn(id, "fresh q").await.unwrap();

    // The raw leaf path (from the ConversationStore directly) MUST contain
    // COMPACTION_TAIL_AUTHOR nodes — that is how the model-visible view forms a
    // contiguous suffix after restart.
    let raw_nodes = sessions.store.leaf_path(id, None).await.unwrap();
    let raw_tail_copies: Vec<_> = raw_nodes
        .iter()
        .filter(|n| n.author.is_compaction_tail_copy())
        .collect();
    assert!(
        !raw_tail_copies.is_empty(),
        "raw leaf path must contain tail copies on disk"
    );

    // The rendered transcript must NOT contain any tail copies.
    let history = sessions.history_nodes(id).await.unwrap();
    let tail_copies_in_history: Vec<_> = history
        .iter()
        .filter(|n| n.author.is_compaction_tail_copy())
        .collect();
    assert!(
        tail_copies_in_history.is_empty(),
        "history_nodes must filter all tail copies"
    );

    // The original tail content must still appear (once) in history — its
    // canonical copy sits before the marker and is not a tail copy.
    let history_msgs = sessions.history(id).await.unwrap();
    let tail_q_count = history_msgs
        .iter()
        .filter(|m| m.content == "tail Q")
        .count();
    assert_eq!(tail_q_count, 1, "tail Q originals present once in history");
}
