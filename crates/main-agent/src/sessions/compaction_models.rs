//! Per-conversation / per-model compaction trigger resolution.

use super::super::*;
use super::test_fixtures::*;

/// Two conversations on models with different absolute triggers compact at different points.
///
/// Drives a real `ChatSessions` + durable store. The fixture deliberately sets per-model triggers
/// (as the server does at boot from window sizes) — a single shared `trigger_tokens` cannot pass.
#[tokio::test]
async fn two_conversations_on_different_models_compact_at_different_thresholds() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    // Enough scripted replies: seed turns for two chats + compact summarizer + post-compact for
    // the small-model chat; big-model chat only needs its final turn reply (no summarizer).
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            // seed small: 2 turns
            CompletionResponse::text("s1"),
            CompletionResponse::text("s2"),
            // seed big: 2 turns
            CompletionResponse::text("b1"),
            CompletionResponse::text("b2"),
            // small: summarizer + turn reply
            CompletionResponse::text("SUMMARY: small-window rolled up"),
            CompletionResponse::text("small-model answer"),
            // big: turn only (under its higher trigger)
            CompletionResponse::text("big-model answer"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());

    // Per-model thresholds as server would pre-resolve from [[models]] windows.
    // Seed with a high default so history builds without compacting; then pin models.
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("model-64k".into(), 1u32); // always fire once selected
    model_triggers.insert("model-200k".into(), 1_000_000u32); // never fire on this fixture
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1_000_000, // daemon default: high while seeding
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 1_000_000,
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );

    let small = sessions.create(None).await.unwrap();
    let big = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        small,
        &[("u1 secret-alpha", "s1"), ("u2 keep", "s2")],
    )
    .await;
    seed_turns(
        &sessions,
        big,
        &[("u1 secret-alpha", "b1"), ("u2 keep", "b2")],
    )
    .await;

    // Pin models after seed so the compact decision uses per-model thresholds.
    sessions.select_model(small, "model-64k".into());
    sessions.select_model(big, "model-200k".into());

    assert_eq!(
        sessions.compaction_trigger_for_session(small).await,
        Some(1),
        "64k model must resolve to its own low trigger"
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(big).await,
        Some(1_000_000),
        "200k model must resolve to its own high trigger"
    );
    assert_ne!(
        sessions.compaction_trigger_for_session(small).await,
        sessions.compaction_trigger_for_session(big).await,
        "two models must not share one threshold"
    );

    sessions.turn(small, "after pin small").await.unwrap();
    sessions.turn(big, "after pin big").await.unwrap();

    let hist_small = sessions.history(small).await.unwrap();
    let hist_big = sessions.history(big).await.unwrap();
    assert!(
        hist_small
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "low-threshold conversation must compact; history={hist_small:?}"
    );
    assert!(
        !hist_big
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "high-threshold conversation must NOT compact on the same history size; history={hist_big:?}"
    );
    // Elided secret must leave the small model's next request, not the big one's raw path.
    assert!(
        !hist_small
            .iter()
            .any(|m| m.content.contains("secret-alpha"))
            || hist_small
                .iter()
                .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "small chat compacted (marker present)"
    );
}

/// Conversations with no model of their own use the daemon-default trigger (pre–per-conversation
/// model behaviour).
#[tokio::test]
async fn conversation_without_model_uses_daemon_default_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("ok")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("pinned".into(), 42u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 12_345,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 99,
            ..CompactionConfig::default()
        },
        provider,
    );

    let unpinned = sessions.create(None).await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(unpinned).await,
        Some(12_345),
        "no model → daemon default"
    );

    let pinned = sessions.create(None).await.unwrap();
    sessions.select_model(pinned, "pinned".into());
    assert_eq!(
        sessions.compaction_trigger_for_session(pinned).await,
        Some(42),
        "pending per-conversation model → table entry"
    );
}

/// The assertion that would have caught the bug: daemon-wide face-model resync must not retune a
/// conversation that has its own model.
#[tokio::test]
async fn daemon_wide_resync_does_not_retune_conversation_with_own_model() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("seed"),
            CompletionResponse::text("still-here"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("conv-model".into(), 7_777u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 48_000,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 48_000,
            ..CompactionConfig::default()
        },
        provider,
    );

    let pinned = sessions.create(None).await.unwrap();
    // Stamp the model on the log so resolution is durable, not only pending.
    sessions.select_model(pinned, "conv-model".into());
    sessions.turn(pinned, "hello").await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(pinned).await,
        Some(7_777)
    );

    let unpinned = sessions.create(None).await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(unpinned).await,
        Some(48_000)
    );

    // Simulate resync_compaction_trigger_for_face_model after POST /api/models/select (daemon-wide).
    sessions.set_compaction_trigger_tokens(1_111);
    assert_eq!(
        sessions.compaction_trigger_tokens(),
        Some(1_111),
        "default updates"
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(unpinned).await,
        Some(1_111),
        "unpinned chats follow the new daemon default"
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(pinned).await,
        Some(7_777),
        "pinned conversation must keep its model trigger after daemon-wide resync — \
         this is the assertion that would have caught the shared-number bug"
    );
}

/// `compaction_trigger_for_session` resolves through the per-model table rather than the daemon
/// default.
///
/// Scope note, because this test used to claim more than it does: it exercises the **query**, not
/// `maybe_compact`. Replacing `for_model(turn_model)` with `.default` inside `maybe_compact` leaves
/// this test passing — verified. The check that catches that mutation is
/// [`two_conversations_on_different_models_compact_at_different_thresholds`], which drives real
/// turns and asserts one conversation grew a summary marker while the other did not.
#[tokio::test]
async fn per_conversation_trigger_query_reads_the_model_table() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("x")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    // Distinct from default so a default-only path cannot accidentally pass.
    model_triggers.insert("wired-model".into(), 55_555u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 11_111,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 22_222,
            ..CompactionConfig::default()
        },
        provider,
    );

    let id = sessions.create(None).await.unwrap();
    sessions.select_model(id, "wired-model".into());
    let effective = sessions
        .compaction_trigger_for_session(id)
        .await
        .expect("compaction wired");
    assert_eq!(
        effective, 55_555,
        "must use model_trigger_tokens[wired-model], not default 11111 — \
         deleting per-conversation resolution fails this test"
    );
    assert_ne!(effective, sessions.compaction_trigger_tokens().unwrap());
}

/// Asking which threshold a conversation is on must not steal the pending model pick from the
/// turn that follows.
///
/// The query and the turn share one precedence chain; taking the pick is the single intentional
/// difference between them. If the query consumed it, the pick would be silently dropped and the
/// turn would fall back to the daemon default — the exact class of bug this deliverable exists to
/// fix, reintroduced through the instrument built to measure it.
#[tokio::test]
async fn asking_for_the_trigger_does_not_consume_the_pending_model_pick() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [CompletionResponse::text("answered")],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let mut model_triggers = std::collections::HashMap::new();
    model_triggers.insert("picked-model".into(), 31_337u32);
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1_000_000,
            model_trigger_tokens: model_triggers,
            unknown_model_trigger_tokens: 1_000_000,
            ..CompactionConfig::default()
        },
        provider,
    );

    let id = sessions.create(None).await.unwrap();
    sessions.select_model(id, "picked-model".into());

    // Ask twice — a consuming peek would already have lost the pick by the second call.
    assert_eq!(
        sessions.compaction_trigger_for_session(id).await,
        Some(31_337)
    );
    assert_eq!(
        sessions.compaction_trigger_for_session(id).await,
        Some(31_337),
        "the pick must survive being asked about"
    );

    // And the turn still runs on it. Afterwards the pending pick is gone, so resolution falls to
    // the log — still the picked model only if the turn actually received it and stamped it.
    sessions.turn(id, "hello").await.unwrap();
    assert_eq!(
        sessions.compaction_trigger_for_session(id).await,
        Some(31_337),
        "the turn must have received the pick the query left alone, and stamped it on the log"
    );
}
