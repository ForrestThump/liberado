//! Latency correlation on `ChatSessions::turn` and compaction.

use super::super::*;
use super::test_fixtures::*;

// ── Correlation coverage (round-2 §1) ──────────────────────────────────────
//
// Boundary under test: `ChatSessions::turn` / `turn_stream` set the latency task-local so
// `MeteredProvider` records `LatencyEvent.correlation` as the session id. R3 — observe what the
// recorder saw, not that a wrap function was called. R1 — deleting the wrap in `turn` fails
// `turn_records_session_id_as_latency_correlation` (mutation evidence in the PR).

/// R3: a turn started through `ChatSessions::turn` records the conversation id on every
/// `LatencyEvent`, not `"-"`. Asserts structured `event.correlation`, not a substring of a dump.
///
/// R1: if the `with_correlation` wrap on `ChatSessions::turn` is removed, this fails with
/// `left: "-"` / `right: <session ulid>`. That failure is the proof the wrap is load-bearing.
#[tokio::test]
async fn turn_records_session_id_as_latency_correlation() {
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(CapturingRecorder::default());
    let sessions = metered_sessions_at(
        dir.path(),
        vec![CompletionResponse::text("attributed reply")],
        Arc::clone(&rec),
    )
    .await;
    let id = sessions.create(None).await.unwrap();

    sessions
        .turn(id, "hello from non-streaming path")
        .await
        .unwrap();

    let events = rec.events.lock().unwrap();
    assert!(
        !events.is_empty(),
        "MeteredProvider must have recorded at least one llm_call"
    );
    let expected = id.to_string();
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(
            ev.correlation, expected,
            "event[{i}] correlation must be the session id (not \"-\"); kind={}",
            ev.kind
        );
    }
}

/// `"-" still means genuinely uncorrelated`: a metered complete outside any `with_correlation`
/// scope must not invent a conversation id. Guards the landmine of defaulting correlation inside
/// `MeteredProvider`.
#[tokio::test]
async fn metered_call_outside_turn_still_records_dash() {
    let rec = Arc::new(CapturingRecorder::default());
    let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("lonely")],
    ));
    let recorder = rec.clone() as Arc<dyn LatencyRecorder>;
    let metered = MeteredProvider::new(inner, AgentRole::Face, recorder);

    metered
        .complete(CompletionRequest::new(vec![Message::user("no scope")]))
        .await
        .unwrap();

    let events = rec.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].correlation, "-",
        "outside any scope the journal must keep the visible gap, not fabricate an id"
    );
}

/// Compaction summarisation runs inside the same `with_correlation` future as the turn, so when
/// the summarizer is a `MeteredProvider` both the summary call and the face completion share the
/// session id.
///
/// Title seeding (`maybe_seed_default_title`) is **not** a model call — it writes the first user
/// line into the store header. There is nothing for correlation to attribute; documented here so
/// the acceptance item is covered honestly (R5) rather than with a vacuous test.
#[tokio::test]
async fn compaction_summariser_inherits_turn_correlation() {
    let dir = tempfile::tempdir().unwrap();
    let rec = Arc::new(CapturingRecorder::default());
    let summary = "SUMMARY: earlier chatter".to_string();
    // Same trigger sizing pattern as `compacts_over_trigger_*`: seed far over threshold.
    let pad = "x".repeat(400);
    let trigger = compaction::estimate_tokens(&[
        Message::system(DEFAULT_SYSTEM_PROMPT),
        compaction::marker_message(&summary),
        Message::user("tail"),
        Message::assistant("ok"),
        Message::user("incoming"),
    ]);
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: trigger,
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
        ..CompactionConfig::default()
    };

    let store = Arc::new(SessionStore::open(dir.path()).await);
    let inner: Arc<dyn Provider> = Arc::new(MockProvider::with_script(
        "mock",
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("post-compact answer"),
        ],
    ));
    // One metered provider shared by executor + summarizer so every hop hits the same recorder.
    let recorder = rec.clone() as Arc<dyn LatencyRecorder>;
    let provider = MeteredProvider::wrap(inner, AgentRole::Face, recorder);
    let executor = Executor::new(Arc::clone(&provider), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools))
        .with_compaction(config, Arc::clone(&provider));

    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            (&format!("secret-A {pad}"), "a1"),
            (&format!("secret-B {pad}"), "a2"),
            (&format!("secret-C {pad}"), "a3"),
        ],
    )
    .await;

    sessions.turn(id, "what was that about?").await.unwrap();

    let events = rec.events.lock().unwrap();
    assert!(
        events.len() >= 2,
        "expected summarizer + turn completions, got {}",
        events.len()
    );
    let expected = id.to_string();
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(
            ev.correlation, expected,
            "event[{i}] must inherit the turn's session correlation; got {:?}",
            ev.correlation
        );
    }
}
