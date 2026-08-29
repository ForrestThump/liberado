//! Compaction fire, trigger sizing, and summarizer failure.

use super::super::*;
use super::test_fixtures::*;

#[tokio::test]
async fn compacts_over_trigger_and_the_next_turn_sees_the_summary_not_the_raw_history() {
    let dir = tempfile::tempdir().unwrap();
    let summary = "SUMMARY: earlier chit-chat about squirrels".to_string();

    // Sized so the seeded history (four ~600-char messages) far exceeds it, while the
    // post-compaction view (system + marker + short tail + short first turn + the next incoming
    // question) lands exactly AT it — so turn 1 compacts and turn 2 provably does not.
    let trigger = compaction::estimate_tokens(&[
        Message::system(DEFAULT_SYSTEM_PROMPT),
        compaction::marker_message(&summary),
        Message::user("tail question"),
        Message::assistant("tail answer"),
        Message::user("fresh question"),
        Message::assistant("fresh answer"),
        Message::user("second question"),
    ]);
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: trigger,
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("fresh answer"),
            CompletionResponse::text("second answer"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    let secret = format!("SECRET-ELIDED-{}", "x".repeat(600));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret, &format!("A1 {}", "y".repeat(600))),
            (
                &format!("u2 {}", "z".repeat(600)),
                &format!("A2 {}", "w".repeat(600)),
            ),
            ("tail question", "tail answer"),
        ],
    )
    .await;

    // Turn 1: over the trigger → summarize, persist the marker, run on the compacted view.
    let reply = sessions.turn(id, "fresh question").await.unwrap();
    assert_eq!(reply, "fresh answer");

    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "summarizer + one turn completion");
    // The summarizer's input is where the elided content legitimately goes…
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-ELIDED")),
        "the elided region must reach the summarizer's transcript"
    );
    // …and the turn request is where it must NOT go. This is the assertion that fails if
    // compaction is broken into a no-op: the raw history would ride every request forever.
    let turn_req = &requests[1];
    assert!(
        turn_req.messages.iter().any(|m| m
            .content
            .contains("SUMMARY: earlier chit-chat about squirrels")),
        "the compacted view must carry the rolling summary"
    );
    assert!(
        !turn_req
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-ELIDED")),
        "elided history must not reach the model after compaction"
    );
    assert!(
        turn_req
            .messages
            .iter()
            .any(|m| m.content == "tail question")
            && turn_req
                .messages
                .iter()
                .any(|m| m.content == "fresh question"),
        "the kept tail and the incoming message must survive verbatim"
    );

    // The full rendered history keeps EVERYTHING — marker included, raw elided content intact
    // (compaction never deletes; it only changes what the model sees).
    let history = sessions.history(id).await.unwrap();
    assert!(history.iter().any(|m| m.content.contains("SECRET-ELIDED")));
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "rendered history must show the compaction marker"
    );

    // Turn 2: under the trigger now, so no second summarization — and the marker persisted, so
    // the next load still resumes from the summary, not the raw history.
    let reply2 = sessions.turn(id, "second question").await.unwrap();
    assert_eq!(reply2, "second answer");
    let requests = provider.received_requests();
    assert_eq!(
        requests.len(),
        3,
        "no second summarization should have run (view is under the trigger)"
    );
    let turn2 = &requests[2];
    assert!(
        turn2.messages.iter().any(|m| m
            .content
            .contains("SUMMARY: earlier chit-chat about squirrels")),
        "the marker must persist across loads"
    );
    assert!(
        !turn2
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-ELIDED")),
        "the elision rule must hold on the next load too"
    );
}

/// Rolling compaction: a second fire must fold the previous marker into the summarizer transcript
/// and replace it with a new summary — not re-summarize only the post-marker slice and drop prior
/// facts. Break-check for the "rolling update" claim that was previously only live-verified.
#[tokio::test]
async fn second_compaction_rolls_prior_summary_forward() {
    let dir = tempfile::tempdir().unwrap();
    let summary_a = "SUMMARY-A: code word is ALPHA".to_string();
    let summary_b = "SUMMARY-B: code word is ALPHA; later topic is BETA".to_string();

    // Always-fire trigger so both turns compact; keep_recent_turns=1 so each fire has something to
    // elide once we have grown past a single user turn after the previous marker.
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1,
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        tool_result_max_chars: 2_000,
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary_a.clone()),
            CompletionResponse::text("reply after first compact"),
            CompletionResponse::text(summary_b.clone()),
            CompletionResponse::text("reply after second compact"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    let secret_a = format!("SECRET-A-{}", "a".repeat(80));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret_a, "assistant about alpha"),
            ("mid turn", "mid answer"),
            ("tail-1 question", "tail-1 answer"),
        ],
    )
    .await;

    // Compaction 1: elides SECRET-A region; model sees summary A + kept tail.
    let reply1 = sessions.turn(id, "after first compact").await.unwrap();
    assert_eq!(reply1, "reply after first compact");

    // Grow past the post-compact suffix so the next turn has material to roll forward — including
    // a second secret that must only reach the *second* summarizer, not the final turn request.
    let secret_b = format!("SECRET-B-{}", "b".repeat(80));
    seed_turns(
        &sessions,
        id,
        &[
            (&secret_b, "assistant about beta"),
            ("tail-2 question", "tail-2 answer"),
        ],
    )
    .await;

    // Compaction 2: rolling update — prior summary + new secrets go to the summarizer; turn sees B.
    let reply2 = sessions.turn(id, "after second compact").await.unwrap();
    assert_eq!(reply2, "reply after second compact");

    let requests = provider.received_requests();
    assert_eq!(
        requests.len(),
        4,
        "summarizer1 + turn1 + summarizer2 + turn2"
    );

    let summarizer2 = &requests[2];
    let summarizer2_blob: String = summarizer2
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        summarizer2_blob.contains(compaction::SUMMARY_HEADER)
            && summarizer2_blob.contains("SUMMARY-A: code word is ALPHA"),
        "second summarizer must see the prior rolling summary to fold it forward, got:\n{summarizer2_blob}"
    );
    assert!(
        summarizer2_blob.contains("SECRET-B-"),
        "second summarizer must see post-marker secrets being folded in"
    );

    let turn2 = &requests[3];
    assert!(
        turn2.messages.iter().any(|m| m
            .content
            .contains("SUMMARY-B: code word is ALPHA; later topic is BETA")),
        "second compacted view must carry the new rolling summary"
    );
    assert!(
        !turn2
            .messages
            .iter()
            .any(|m| m.content.contains("SECRET-A-") || m.content.contains("SECRET-B-")),
        "neither generation's raw secrets may reach the model after the second compaction"
    );
    // The first summary text itself should have been superseded by summary B in the model view
    // (the old marker is in the elided region / folded into B).
    assert!(
        !turn2
            .messages
            .iter()
            .any(|m| m.content.contains("SUMMARY-A: code word is ALPHA")
                && !m.content.contains("SUMMARY-B")),
        "stale summary A must not ride the post-second-compaction turn as the active marker"
    );
    assert!(
        turn2
            .messages
            .iter()
            .any(|m| m.content == "tail-2 question")
            && turn2
                .messages
                .iter()
                .any(|m| m.content == "after second compact"),
        "kept tail and incoming user message must survive the second compaction"
    );

    // Two markers on the durable transcript (append-only); rendered history keeps both.
    let history = sessions.history(id).await.unwrap();
    let markers = history
        .iter()
        .filter(|m| m.content.starts_with(compaction::SUMMARY_HEADER))
        .count();
    assert_eq!(
        markers, 2,
        "each compaction must leave a durable marker node"
    );
}

/// Store that injects a single `append` failure for a node whose content equals `fail_once_content`,
/// then delegates forever. Used to exercise partial tail re-append after the marker is written.
struct FailOnceContentStore {
    inner: Arc<SessionStore>,
    fail_once_content: std::sync::Mutex<Option<String>>,
    fail_count: AtomicUsize,
}

#[async_trait]
impl ConversationStore for FailOnceContentStore {
    async fn create(
        &self,
        new: liberado_conversation_store::NewConversation,
    ) -> liberado_conversation_store::StoreResult<liberado_conversation_store::ConversationHeader>
    {
        self.inner.create(new).await
    }

    async fn append(
        &self,
        conversation: Ulid,
        node: NewNode,
    ) -> liberado_conversation_store::StoreResult<MessageNode> {
        let should_fail = {
            let mut guard = self.fail_once_content.lock().unwrap();
            if guard.as_ref() == Some(&node.message.content) {
                *guard = None;
                true
            } else {
                false
            }
        };
        if should_fail {
            self.fail_count.fetch_add(1, AtomicOrdering::SeqCst);
            return Err(StoreError::Io(std::io::Error::other(
                "injected tail re-append failure",
            )));
        }
        self.inner.append(conversation, node).await
    }

    async fn leaf_path(
        &self,
        conversation: Ulid,
        leaf: Option<Ulid>,
    ) -> liberado_conversation_store::StoreResult<Vec<MessageNode>> {
        self.inner.leaf_path(conversation, leaf).await
    }

    async fn node(
        &self,
        conversation: Ulid,
        id: Ulid,
    ) -> liberado_conversation_store::StoreResult<Option<MessageNode>> {
        self.inner.node(conversation, id).await
    }

    async fn children(
        &self,
        conversation: Ulid,
        id: Ulid,
    ) -> liberado_conversation_store::StoreResult<Vec<Ulid>> {
        self.inner.children(conversation, id).await
    }

    async fn list(
        &self,
    ) -> liberado_conversation_store::StoreResult<
        Vec<liberado_conversation_store::ConversationHeader>,
    > {
        self.inner.list().await
    }

    async fn header(
        &self,
        conversation: Ulid,
    ) -> liberado_conversation_store::StoreResult<liberado_conversation_store::ConversationHeader>
    {
        self.inner.header(conversation).await
    }

    async fn set_title(
        &self,
        conversation: Ulid,
        title: String,
    ) -> liberado_conversation_store::StoreResult<()> {
        self.inner.set_title(conversation, title).await
    }

    async fn set_grant(
        &self,
        conversation: Ulid,
        grant: liberado_session::SessionGrant,
    ) -> liberado_conversation_store::StoreResult<()> {
        self.inner.set_grant(conversation, grant).await
    }

    async fn delete(&self, conversation: Ulid) -> liberado_conversation_store::StoreResult<()> {
        self.inner.delete(conversation).await
    }
}

/// If a tail re-append fails after the marker is durable, this turn must still see the full kept
/// tail (not a truncated in-memory view). Persistence of that one node may still be incomplete —
/// that is the inherent limit without multi-node transactions — but the break-early bug that also
/// stripped remaining tail from *this turn's* conversation is what we guard against.
#[tokio::test]
async fn partial_tail_reappend_failure_keeps_full_view_for_this_turn() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FailOnceContentStore {
        inner: Arc::new(SessionStore::open(dir.path()).await),
        // Armed *after* seed so the original "tail answer" node can land; only the compaction
        // re-append of that content is injected to fail.
        fail_once_content: std::sync::Mutex::new(None),
        fail_count: AtomicUsize::new(0),
    });
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("SUMMARY: partial-tail test"),
            CompletionResponse::text("still answered"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store.clone(), executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1,
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 secret", "a1"),
            ("u2", "a2"),
            ("tail question", "tail answer"),
        ],
    )
    .await;
    // Fail the re-append of the assistant half of the kept tail (keep_recent_turns=1 →
    // user+assistant). Marker and the user half of the re-tail succeed; this one fails once.
    *store.fail_once_content.lock().unwrap() = Some("tail answer".into());

    let reply = sessions.turn(id, "fresh question").await.unwrap();
    assert_eq!(reply, "still answered");
    assert_eq!(
        store.fail_count.load(AtomicOrdering::SeqCst),
        1,
        "the injected failure must have fired exactly once"
    );

    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "summarizer + turn");
    let turn_req = &requests[1];
    assert!(
        turn_req
            .messages
            .iter()
            .any(|m| m.content == "tail question")
            && turn_req.messages.iter().any(|m| m.content == "tail answer")
            && turn_req
                .messages
                .iter()
                .any(|m| m.content == "fresh question"),
        "this turn's model view must include the full kept tail even when one re-append failed; got: {:?}",
        turn_req
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        turn_req
            .messages
            .iter()
            .any(|m| m.content.contains("SUMMARY: partial-tail test")),
        "marker must still be in the compacted view"
    );
}

#[tokio::test]
async fn stream_turn_also_compacts() {
    // The streaming path (webui/TUI/CLI) shares `maybe_compact` with `turn` — prove it.
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1, // always fires
        keep_recent_turns: 1,
        ..CompactionConfig::default()
    };
    let (sessions, _provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("SUMMARY: old stuff"),
            CompletionResponse::text("streamed answer"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[("u1", "a1"), ("u2", "a2"), ("tail q", "tail a")],
    )
    .await;

    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    sessions.turn_stream(id, "stream me", &tx).await.unwrap();
    drop(tx);
    let mut tokens = String::new();
    while let Some(ev) = rx.recv().await {
        if let AgentEvent::Token(t) = ev {
            tokens.push_str(&t);
        }
    }
    assert_eq!(tokens, "streamed answer");
    let history = sessions.history(id).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "the streaming path must persist the marker too"
    );
}

#[tokio::test]
async fn no_compaction_under_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: 1_000_000,
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("r1"),
            CompletionResponse::text("r2"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    sessions.turn(id, "first thing").await.unwrap();
    sessions.turn(id, "second thing").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "no marker without a compaction"
    );
    // Ordinary rehydration: turn 2's request still carries turn 1 verbatim.
    let requests = provider.received_requests();
    assert_eq!(requests.len(), 2, "no summarizer call under the trigger");
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|m| m.content == "first thing"),
        "under-trigger history must ride along untouched"
    );
}

#[tokio::test]
async fn disabled_config_never_compacts() {
    let dir = tempfile::tempdir().unwrap();
    let config = CompactionConfig {
        enabled: false,
        trigger_tokens: 1, // would always fire if enabled
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text("r1"),
            CompletionResponse::text("r2"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(&sessions, id, &[("u1", "a1"), ("u2", "a2")]).await;
    sessions.turn(id, "third thing").await.unwrap();

    let history = sessions.history(id).await.unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "a disabled config must never write markers"
    );
    assert_eq!(provider.received_requests().len(), 1);
    assert!(
        provider.received_requests()[0]
            .messages
            .iter()
            .any(|m| m.content == "u1"),
        "disabled compaction passes history through untouched"
    );
}

/// A provider that fails its first completion (the summarizer) and delegates the rest to an inner
/// mock — the summarizer-failure path must degrade to running the turn uncompacted.
struct FailOnceProvider {
    inner: MockProvider,
    failed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl Provider for FailOnceProvider {
    fn model(&self) -> String {
        self.inner.model()
    }
    async fn complete(&self, request: CompletionRequest) -> ProviderResult<CompletionResponse> {
        if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(ProviderError::Transport("summarizer boom".into()));
        }
        self.inner.complete(request).await
    }
}

#[tokio::test]
async fn summarizer_failure_runs_the_turn_uncompacted() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(FailOnceProvider {
        inner: MockProvider::with_script("mock", [CompletionResponse::text("uncompacted answer")]),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            trigger_tokens: 1, // always fires
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );
    let id = sessions.create(None).await.unwrap();
    seed_turns(&sessions, id, &[("u1 secret", "a1"), ("u2", "a2")]).await;

    // The turn must SUCCEED despite the summarizer failing — compaction may never cost the human
    // their turn.
    let reply = sessions.turn(id, "still answer me").await.unwrap();
    assert_eq!(reply, "uncompacted answer");

    let history = sessions.history(id).await.unwrap();
    assert!(
        !history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "a failed summarization must not persist a marker"
    );
}

#[tokio::test]
async fn set_compaction_trigger_tokens_updates_live_threshold() {
    // Hot-swap path: boot with a high trigger, then lower it as if a smaller-window model was
    // selected — the next turn must compact under the new threshold.
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SessionStore::open(dir.path()).await);
    let provider = Arc::new(MockProvider::with_script(
        "mock",
        [
            CompletionResponse::text("SUMMARY: after swap"),
            CompletionResponse::text("post-compact answer"),
        ],
    ));
    let executor = Executor::new(provider.clone(), Budget::default());
    let sessions = ChatSessions::new(store, executor, Arc::new(NoTools)).with_compaction(
        CompactionConfig {
            enabled: true,
            // High enough that seed turns alone won't fire until we lower the live threshold.
            trigger_tokens: 1_000_000,
            keep_recent_turns: 1,
            ..CompactionConfig::default()
        },
        provider.clone(),
    );
    assert_eq!(sessions.compaction_trigger_tokens(), Some(1_000_000));

    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            ("u1 secret-alpha", "a1"),
            ("u2 secret-beta", "a2"),
            ("u3 keep-me", "a3"),
        ],
    )
    .await;

    // Lower the live *default* threshold as resync_compaction_trigger_for_face_model does.
    // This conversation has no model of its own, so it observes the new default.
    sessions.set_compaction_trigger_tokens(1);
    assert_eq!(sessions.compaction_trigger_tokens(), Some(1));
    assert_eq!(sessions.compaction_trigger_for_session(id).await, Some(1));

    let reply = sessions.turn(id, "after swap").await.unwrap();
    assert_eq!(reply, "post-compact answer");

    let history = sessions.history(id).await.unwrap();
    assert!(
        history
            .iter()
            .any(|m| m.content.starts_with(compaction::SUMMARY_HEADER)),
        "after lowering the live trigger, the next turn must compact"
    );
}

/// Compaction fires when history + the incoming message crosses THIS conversation's trigger.
/// Sizing the trigger one token below the combined estimate makes `+` compact and `-`
/// pass through — the swap survived every coarse assertion before this test.
#[tokio::test]
async fn incoming_user_message_counts_toward_the_compaction_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let summary = "SUMMARY: folded past".to_string();

    // Seed two long-ish turns, then size the trigger so that:
    //   estimate(seed + system + incoming) == T + 1  → `+` crosses (compacts)
    //   estimate(seed + system) - estimate(incoming) < T → `-` does not
    let seeded = vec![
        Message::system(crate::DEFAULT_SYSTEM_PROMPT),
        Message::user(format!("u1 {}", "x".repeat(400))),
        Message::assistant(format!("a1 {}", "y".repeat(400))),
        Message::user(format!("u2 {}", "z".repeat(400))),
        Message::assistant(format!("a2 {}", "w".repeat(400))),
    ];
    let incoming_est = compaction::estimate_tokens(&[Message::user("trigger me")]);
    let base = compaction::estimate_tokens(&seeded);
    let trigger = base + incoming_est - 1;

    let config = CompactionConfig {
        enabled: true,
        trigger_tokens: trigger,
        keep_recent_turns: 1,
        summary_max_tokens: 512,
        ..CompactionConfig::default()
    };
    let (sessions, provider) = compacting_sessions_at(
        dir.path(),
        config,
        vec![
            CompletionResponse::text(summary.clone()),
            CompletionResponse::text("ok"),
        ],
    )
    .await;
    let id = sessions.create(None).await.unwrap();
    seed_turns(
        &sessions,
        id,
        &[
            (
                &format!("u1 {}", "x".repeat(400)),
                &format!("a1 {}", "y".repeat(400)),
            ),
            (
                &format!("u2 {}", "z".repeat(400)),
                &format!("a2 {}", "w".repeat(400)),
            ),
        ],
    )
    .await;

    sessions.turn(id, "trigger me").await.unwrap();

    // Two provider calls = the summarizer ran = compaction crossed the threshold.
    let requests = provider.received_requests();
    assert_eq!(
        requests.len(),
        2,
        "history + incoming must cross the trigger and fire the summarizer"
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|m| m.content.contains(&summary)),
        "the turn then runs on the compacted view"
    );
}
