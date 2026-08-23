//! Split from `goals.rs` for module-health boundaries.

//! HTTP-level integration tests for `POST /api/goals/{id}/message`, against a real `axum::Router`
//! wired to a `GoalSessionHub` with the life-ops demo pack (the same pattern as `hooks.rs`).
/// The bound must announce itself. A silently truncated diff reads as a complete one, which is
/// worse than a large response: the human concludes the change set is smaller than it is.
///
/// Scope (R5): this exercises the bounding rule, not the handler — running the real endpoint
/// would need a git workspace carrying a megabyte of uncommitted change.
#[test]
fn an_oversized_diff_is_truncated_and_says_so() {
    let small = "diff --git a/x b/x
+one line
"
    .to_string();
    assert_eq!(
        super::bound_diff(small.clone()),
        small,
        "a diff under the cap must be returned byte-for-byte"
    );

    let huge = "x".repeat(super::MAX_DIFF_BYTES + 5_000);
    let bounded = super::bound_diff(huge);
    assert!(
        bounded.len() < super::MAX_DIFF_BYTES + 500,
        "must actually shrink, got {} bytes",
        bounded.len()
    );
    assert!(
        bounded.contains("diff truncated"),
        "truncation must be visible in the body, not silent"
    );
}

/// Cutting at a fixed byte offset can land mid-codepoint. `String` will not hold an invalid
/// slice, so getting this wrong is a panic on any workspace with non-ASCII in its diff.
#[test]
fn truncation_lands_on_a_char_boundary() {
    // 3 bytes wide, and the cap is not a multiple of 3 — so a naive cut lands *inside* a
    // codepoint. A 2-byte char would align with the even cap and prove nothing.
    assert_ne!(
        super::MAX_DIFF_BYTES % 3,
        0,
        "fixture only bites on a misaligned cap"
    );
    let huge = "€".repeat(super::MAX_DIFF_BYTES);
    let bounded = super::bound_diff(huge);
    assert!(bounded.contains("diff truncated"));
}

use super::*;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use liberado_session::{
    DomainHint, GoalSessionHub, GoalSessionStore, GoalSpec, LifeOpsDemoRunner, SessionSnapshot,
};
use tower::ServiceExt;

/// Build a router exposing just the goal-session routes under test, plus a handle to the hub so
/// a test can poll session state directly (start a session, wait for `awaiting_input`, â€¦).
fn goals_app() -> (Router, Arc<GoalSessionHub>) {
    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let goals = Arc::new(hub);

    let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        dispatcher_attached: false,
        orchestrator_attached: false,
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vault_path: "/tmp/vault".to_string(),
        goals: goals.clone(),
        chat: None,
        chat_tools: 0,
        chat_tool_names: Vec::new(),
        catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
        data_dir: std::path::PathBuf::from("/tmp/liberado"),
        sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
        main_agent_capabilities: liberado_common::CapabilitySet::empty(),
        dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
        config: Arc::new(test_config_with_life_grants()),
        sessions: Arc::new(Default::default()),
        model_name: None,
        provider: None,
        hooks: std::collections::HashMap::new(),
        hook_tx,
        hook_idempotency: crate::hooks::IdempotencyCache::default(),
        live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        drain: crate::shutdown::DrainGate::default(),
    });

    let app = Router::new()
        .route(
            "/api/goals/{id}/message",
            axum::routing::post(goals_message),
        )
        .with_state(state);
    (app, goals)
}

/// Like [`goals_app`] but with a **real** `ChatSessions` (temp JSONL store, `MockProvider` that
/// is never actually called for completions) and the `/api/goals` start route mounted â€” so the
/// return-handoff path can fold a summary into a genuine parent conversation. Returns the router,
/// the hub, the chat handle, and a freshly-created conversation id to use as `origin`.
async fn goals_app_with_chat() -> (
    Router,
    Arc<GoalSessionHub>,
    Arc<liberado_main_agent::ChatSessions>,
    String,
) {
    use liberado_executor::{Budget, Executor};
    use liberado_provider::MockProvider;

    let mut hub = GoalSessionHub::new(GoalSessionStore::new());
    hub.register_pack(Arc::new(LifeOpsDemoRunner));
    let goals = Arc::new(hub);

    let root = std::env::temp_dir().join(format!("liberado-server-test-{}", Ulid::new()));
    let store = Arc::new(liberado_session_store::SessionStore::open(&root).await);
    let executor = Executor::new(
        Arc::new(MockProvider::with_script("mock", vec![])),
        Budget::default(),
    );
    let chat = Arc::new(liberado_main_agent::ChatSessions::new(
        store,
        executor,
        Arc::new(crate::state::NoTools),
    ));
    let conv = chat.create(None).await.unwrap().to_string();

    let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        dispatcher_attached: false,
        orchestrator_attached: false,
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vault_path: "/tmp/vault".to_string(),
        goals: goals.clone(),
        chat: Some(chat.clone()),
        chat_tools: 0,
        chat_tool_names: Vec::new(),
        catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
        data_dir: root.clone(),
        sessions_root: root,
        main_agent_capabilities: liberado_common::CapabilitySet::empty(),
        dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
        config: Arc::new(test_config_with_life_grants()),
        sessions: Arc::new(Default::default()),
        model_name: None,
        provider: None,
        hooks: std::collections::HashMap::new(),
        hook_tx,
        hook_idempotency: crate::hooks::IdempotencyCache::default(),
        live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        drain: crate::shutdown::DrainGate::default(),
    });

    let app = Router::new()
        .route("/api/goals", axum::routing::post(goals_start))
        .route(
            "/api/goals/{id}/message",
            axum::routing::post(goals_message),
        )
        .with_state(state);
    (app, goals, chat, conv)
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// The grant an attended `/spawn` of the life pack resolves to: it may interrupt the human, and
/// it may write the note it was asked for. Interactivity is a capability (S6) — a session
/// started without `AskHuman` cannot receive input at all, so these tests must grant it
/// explicitly rather than relying on an ambient "interactive" payload flag.
fn attended_life_grant() -> liberado_session::SessionGrant {
    liberado_session::SessionGrant {
        capabilities: life_capabilities(),
        profile: None,
        overrides: serde_json::Value::Null,
        ..Default::default()
    }
}

fn grant_with_ask_human() -> liberado_session::SessionGrant {
    use liberado_common::{Capability, CapabilitySet, Zone};
    let mut capabilities = CapabilitySet::empty();
    capabilities.grant(Capability::AskHuman);
    capabilities.grant(Capability::Write(Zone::vault("tasks")));
    liberado_session::SessionGrant {
        capabilities,
        profile: None,
        overrides: serde_json::Value::Null,
        ..Default::default()
    }
}

fn goal_with_interactive(interactive: Option<bool>) -> liberado_session::GoalSpec {
    let payload = match interactive {
        Some(flag) => serde_json::json!({ "interactive": flag }),
        None => serde_json::json!({}),
    };
    GoalSpec {
        id: None,
        description: "shepherd kickback".into(),
        success_criteria: vec![],
        domain: DomainHint::Coding,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload,
    }
}

/// F11: shepherd sends `interactive: false`; the grant must drop AskHuman.
#[test]
fn interactive_false_strips_ask_human_from_the_grant() {
    use liberado_common::{Capability, Zone};
    let grant =
        apply_interactive_to_grant(&goal_with_interactive(Some(false)), grant_with_ask_human());
    assert!(
        !grant.capabilities.grants_ask_human(),
        "unattended goals must not receive AskHuman"
    );
    assert!(
        grant
            .capabilities
            .contains(&Capability::Write(Zone::vault("tasks"))),
        "non-AskHuman capabilities must survive"
    );
}

#[test]
fn interactive_true_keeps_ask_human() {
    let grant =
        apply_interactive_to_grant(&goal_with_interactive(Some(true)), grant_with_ask_human());
    assert!(grant.capabilities.grants_ask_human());
}

#[test]
fn interactive_absent_keeps_profile_grant() {
    let grant = apply_interactive_to_grant(&goal_with_interactive(None), grant_with_ask_human());
    assert!(
        grant.capabilities.grants_ask_human(),
        "absent flag must not silently strip AskHuman"
    );
}

/// Ignoring the flag reintroduces F11: this test fails if the call site is removed.
///
/// The helper definition, its docs, or a call from another handler is not enough. The grant
/// passed to `start_with_grant` must be the value narrowed inside `goals_start`.
#[test]
fn apply_interactive_is_invoked_from_goals_start() {
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/goals.rs"));
    let production = src.split("#[cfg(test)]").next().expect("production");
    let goals_start = production
        .split_once("pub async fn goals_start(")
        .and_then(|(_, tail)| tail.split_once("pub async fn goals_get("))
        .map(|(body, _)| body)
        .expect("production source must contain the goals_start body");
    assert!(
        goals_start.contains("let grant = apply_interactive_to_grant(&goal, grant);"),
        "goals_start must narrow and rebind the grant before start_with_grant"
    );
}

fn life_capabilities() -> liberado_common::CapabilitySet {
    use liberado_common::{Capability, CapabilitySet, Zone};
    let mut capabilities = CapabilitySet::empty();
    capabilities.grant(Capability::AskHuman);
    capabilities.grant(Capability::Write(Zone::vault("tasks")));
    capabilities
}

/// A config whose `"life"` component holds the grant an unprofiled life session resolves to â€”
/// mirroring the shipped `policy.toml`. Without this the HTTP path would resolve *zero*
/// authority and a `/spawn`ed session would (correctly) refuse to ask the human anything, which
/// is precisely the behavior `spawned_session_without_ask_human_never_awaits` pins down.
fn test_config_with_life_grants() -> liberado_bootstrap::Config {
    use liberado_config::Grant;
    let mut config = liberado_bootstrap::Config::default();
    config.policy.grants.push(Grant {
        component: "life".into(),
        capabilities: life_capabilities().capabilities,
    });
    config
}

async fn start_interactive(goals: &Arc<GoalSessionHub>) -> String {
    goals
        .start_with_grant(
            GoalSpec {
                id: None,
                description: "capture a note interactively".into(),
                success_criteria: vec![],
                domain: DomainHint::Life,
                max_turns: 0,
                max_idle_secs: None,
                origin: None,
                profile: None,
                payload: serde_json::json!({ "interactive": true }),
            },
            attended_life_grant(),
        )
        .await
        .unwrap()
}

async fn wait_awaiting(goals: &Arc<GoalSessionHub>, id: &str) {
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        if let Some(snap) = goals.snapshot(id).await
            && snap.session.awaiting_input
        {
            return;
        }
    }
    panic!("session {id} never reached awaiting_input");
}

async fn wait_terminal(goals: &Arc<GoalSessionHub>, id: &str) -> SessionSnapshot {
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let snap = goals.snapshot(id).await.unwrap();
        if snap.session.status.is_terminal() {
            return snap;
        }
    }
    panic!("session {id} did not finish");
}

#[tokio::test]
async fn message_delivers_the_answer_echoes_it_and_returns_202() {
    let (app, goals) = goals_app();
    let id = start_interactive(&goals).await;
    wait_awaiting(&goals, &id).await;

    let response = app
        .oneshot(post_json(
            &format!("/api/goals/{id}/message"),
            r#"{"text": "Weekly Review"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let snap = wait_terminal(&goals, &id).await;
    assert_eq!(
        snap.session.status,
        liberado_session::SessionStatus::Succeeded
    );
    // The endpoint's `send_input` echoed the message into the transcript as `human_input`.
    assert!(snap.events.iter().any(|e| matches!(
        &e.kind,
        liberado_session::SessionEventKind::HumanInput { text } if text == "Weekly Review"
    )));
    // And the answer drove the session outcome.
    assert!(
        snap.session
            .result
            .as_ref()
            .unwrap()
            .summary
            .contains("Weekly Review")
    );
}

#[tokio::test]
async fn message_to_unknown_session_is_404() {
    let (app, _goals) = goals_app();
    let response = app
        .oneshot(post_json(
            "/api/goals/does-not-exist/message",
            r#"{"text": "hello"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn handoff_note_includes_status_summary_artifacts_and_rejoin_hint() {
    use liberado_session::{
        DomainHint, GoalResult, GoalSessionRecord, GoalSpec, SessionStatus, TerminalKind,
    };
    let mut record = GoalSessionRecord::new(GoalSpec {
        id: Some("g_01ABC".into()),
        description: "build a hello CLI".into(),
        success_criteria: vec![],
        domain: DomainHint::Coding,
        max_turns: 0,
        max_idle_secs: None,
        origin: None,
        profile: None,
        payload: serde_json::json!({}),
    });
    record.status = SessionStatus::Succeeded;
    record.result = Some(GoalResult {
        terminal: TerminalKind::Succeeded,
        summary: "wrote src/main.rs".into(),
        artifacts: vec!["src/main.rs".into()],
        diagnostics: serde_json::json!({}),
    });
    let note = format_handoff_note(&record, "g_01ABC");
    assert!(note.contains("[coding session succeeded]"), "note: {note}");
    assert!(note.contains("build a hello CLI"));
    assert!(note.contains("Outcome: wrote src/main.rs"));
    assert!(note.contains("Artifacts: src/main.rs"));
    assert!(note.contains("/join g_01ABC"));
}

#[tokio::test]
async fn message_to_finished_session_is_409() {
    let (app, goals) = goals_app();
    // A session that *could* take input (it holds AskHuman), answered and now terminal. This is
    // the real 409: not "you may not", but "you're too late".
    let id = start_interactive(&goals).await;
    wait_awaiting(&goals, &id).await;
    goals.send_input(&id, "Weekly Review").await.unwrap();
    let _ = wait_terminal(&goals, &id).await;

    let response = app
        .oneshot(post_json(
            &format!("/api/goals/{id}/message"),
            r#"{"text": "too late"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn message_to_a_session_without_ask_human_is_403_not_409() {
    // The S6 distinction the status code has to carry: this session was *never allowed* human
    // input (its grant omits AskHuman), which is an authority answer â€” not the timing answer a
    // 409 gives. Started with the default zero-authority grant, exactly like an unattended cron.
    let (app, goals) = goals_app();
    let id = goals
        .start(GoalSpec {
            id: None,
            description: "unattended goal".into(),
            success_criteria: vec!["done".into()],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({ "interactive": true }),
        })
        .await
        .unwrap();

    let response = app
        .oneshot(post_json(
            &format!("/api/goals/{id}/message"),
            r#"{"text": "let me help"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_session_without_ask_human_never_awaits_even_when_asked_to_be_interactive() {
    // Interactivity is a capability, not a payload flag the caller can assert. Despite
    // `interactive: true`, a zero-authority grant means the pack gets a closed input channel and
    // must finish on its own rather than block on a human who can never reply.
    let (_app, goals) = goals_app();
    let id = goals
        .start(GoalSpec {
            id: None,
            description: "unattended note".into(),
            success_criteria: vec!["done".into()],
            domain: DomainHint::Life,
            max_turns: 0,
            max_idle_secs: None,
            origin: None,
            profile: None,
            payload: serde_json::json!({ "interactive": true }),
        })
        .await
        .unwrap();

    let snap = wait_terminal(&goals, &id).await;
    assert!(
        !snap.session.awaiting_input,
        "a session without AskHuman must never await a human"
    );
    assert!(snap.session.status.is_terminal());
}

#[tokio::test]
async fn origin_session_folds_its_summary_into_the_parent_conversation() {
    // The full S4 return handoff, end to end: POST /api/goals with an origin â†’ interactive
    // session â†’ answer it â†’ on terminal, its summary is appended to the parent conversation.
    let (app, goals, chat, conv) = goals_app_with_chat().await;

    // Spawn an interactive life session linked to the conversation (exactly what `/spawn` posts).
    let body = format!(
        r#"{{"description":"capture a note","domain":"life","payload":{{"interactive":true}},"origin":{{"conversation_id":"{conv}"}}}}"#
    );
    let resp = app
        .clone()
        .oneshot(post_json("/api/goals", &body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = v["session_id"].as_str().unwrap().to_string();

    wait_awaiting(&goals, &id).await;
    let resp = app
        .clone()
        .oneshot(post_json(
            &format!("/api/goals/{id}/message"),
            r#"{"text": "Weekly Review"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    wait_terminal(&goals, &id).await;

    // The handoff watcher appends the summary into the parent conversation (async â€” poll for it).
    let conv_ulid: Ulid = conv.parse().unwrap();
    let mut folded = false;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let history = chat.history(conv_ulid).await.unwrap();
        if history.iter().any(|m| {
            m.content.contains("life session succeeded") && m.content.contains("Weekly Review")
        }) {
            folded = true;
            break;
        }
    }
    assert!(
        folded,
        "return handoff did not fold the session summary into the parent conversation"
    );
}

// â”€â”€ Forking â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// A router with the fork route mounted over a real `SessionStore`, seeded with a chat of
/// `turns` (user, assistant) exchanges. Returns the router, the store, and the conversation id.
async fn fork_app(
    turns: &[(&str, &str)],
) -> (Router, Arc<liberado_session_store::SessionStore>, String) {
    use liberado_conversation_store::{Author, ConversationStore, NewNode};
    use liberado_provider::Message;

    let sessions = Arc::new(liberado_session_store::SessionStore::new());
    let conv = sessions
        .create_session(liberado_session_store::NewSession {
            title: Some("original".into()),
            ..Default::default()
        })
        .await
        .id;

    let mut parent = None;
    for (q, a) in turns {
        let u = sessions
            .append(
                conv,
                NewNode {
                    parent_id: parent,
                    author: Author::User,
                    message: Message::user(*q),
                    model: None,
                },
            )
            .await
            .unwrap();
        let a = sessions
            .append(
                conv,
                NewNode {
                    parent_id: Some(u.id),
                    author: Author::Assistant,
                    message: Message::assistant(*a),
                    model: None,
                },
            )
            .await
            .unwrap();
        parent = Some(a.id);
    }

    let (hook_tx, _hook_rx) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(AppState {
        start_time: Instant::now(),
        reactions: Arc::new(tokio::sync::Mutex::new(Vec::new())),
        dispatcher_attached: false,
        orchestrator_attached: false,
        watcher_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vault_path: "/tmp/vault".to_string(),
        goals: Arc::new(GoalSessionHub::new(GoalSessionStore::new())),
        chat: None,
        chat_tools: 0,
        chat_tool_names: Vec::new(),
        catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
        data_dir: std::path::PathBuf::from("/tmp/liberado"),
        sessions_root: std::path::PathBuf::from("/tmp/liberado/sessions"),
        main_agent_capabilities: liberado_common::CapabilitySet::empty(),
        dispatcher_capabilities: liberado_common::CapabilitySet::empty(),
        config: Arc::new(test_config_with_life_grants()),
        sessions: sessions.clone(),
        model_name: None,
        provider: None,
        hooks: std::collections::HashMap::new(),
        hook_tx,
        hook_idempotency: crate::hooks::IdempotencyCache::default(),
        live_mcp: liberado_bootstrap::LiveMcpController::empty(),
        drain: crate::shutdown::DrainGate::default(),
    });

    let app = Router::new()
        .route(
            "/api/sessions/{id}/fork",
            axum::routing::post(crate::api::session_fork),
        )
        .with_state(state);
    (app, sessions, conv.to_string())
}

async fn post_fork(app: &Router, conv: &str, body: serde_json::Value) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{conv}/fork"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn forking_a_whole_conversation_snapshots_it_and_leaves_the_original_alone() {
    let (app, store, conv) = fork_app(&[("q1", "a1"), ("q2", "a2")]).await;

    let (status, body) = post_fork(&app, &conv, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(fork.kept_turns, 2);
    assert_eq!(fork.total_turns, 2);

    use liberado_conversation_store::ConversationStore;
    let fork_id: Ulid = fork.id.parse().unwrap();
    let copied = store.leaf_path(fork_id, None).await.unwrap();
    assert_eq!(
        copied
            .iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["q1", "a1", "q2", "a2"],
    );
    // The original still exists, unchanged, alongside the fork â€” that is the whole request.
    let original: Ulid = conv.parse().unwrap();
    assert_eq!(store.leaf_path(original, None).await.unwrap().len(), 4);
    assert_eq!(store.list_sessions().await.len(), 2);
}

#[tokio::test]
async fn forking_after_a_turn_resolves_that_turn_to_the_right_node() {
    // The server's whole job here: a human points at a *turn*; the store speaks *nodes*.
    // `after_turn: 1` must keep q1 and the answer it got, and drop everything from q2 onward.
    let (app, store, conv) = fork_app(&[("q1", "a1"), ("q2", "a2"), ("q3", "a3")]).await;

    let (status, body) = post_fork(&app, &conv, serde_json::json!({ "after_turn": 1 })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(fork.kept_turns, 1);
    assert_eq!(fork.total_turns, 3);

    use liberado_conversation_store::ConversationStore;
    let copied = store
        .leaf_path(fork.id.parse().unwrap(), None)
        .await
        .unwrap();
    assert_eq!(
        copied
            .iter()
            .map(|n| n.message.content.clone())
            .collect::<Vec<_>>(),
        vec!["q1", "a1"],
        "the reply to turn 1 comes along; turn 2 onward does not"
    );
}

#[tokio::test]
async fn forking_past_the_last_turn_is_the_whole_conversation_not_an_error() {
    // Asking to keep more turns than exist is not a mistake worth refusing â€” it is just "all of
    // it", which is what a bare /fork means anyway.
    let (app, _store, conv) = fork_app(&[("q1", "a1")]).await;
    let (status, body) = post_fork(&app, &conv, serde_json::json!({ "after_turn": 99 })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fork: chat_client_contract::ForkResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(fork.kept_turns, 1);
    assert_eq!(fork.total_turns, 1);
}

#[tokio::test]
async fn forking_turn_zero_is_refused_rather_than_silently_meaning_something_else() {
    let (app, _store, conv) = fork_app(&[("q1", "a1")]).await;
    let (status, body) = post_fork(&app, &conv, serde_json::json!({ "after_turn": 0 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("1-based"), "{body}");
}

#[tokio::test]
async fn forking_an_unknown_session_is_404() {
    let (app, _store, _conv) = fork_app(&[("q1", "a1")]).await;
    let (status, _) = post_fork(&app, &Ulid::new().to_string(), serde_json::json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
