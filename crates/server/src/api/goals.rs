//! Goal-session endpoints (start, stream, cancel, message, handoff).

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, Sse},
    },
};
use futures::Stream;
use liberado_conversation_store::Ulid;
use serde::Deserialize;

use chat_client_contract::ApiError;

use crate::state::AppState;

/// The SSE item stream type shared with chat streaming.
type SseBody = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;
// â”€â”€ Goal sessions (scratchpad F) â€” surfaces are clients; packs own the loop â”€â”€

/// `GET /api/goals/domains` â€” which domain packs are registered (coding, life, â€¦).
pub async fn goals_domains(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(serde_json::json!({
        "domains": state.goals.registered_domains(),
    }))
}

/// `GET /api/goals` â€” list goal sessions, newest first.
pub async fn goals_list(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(state.goals.list().await)
}

/// `POST /api/goals` â€” start a goal session. Body: [`liberado_session::GoalSpec`]. When the spec
/// carries an `origin` (a chat turn spawned it, e.g. via `/spawn`), a **return-handoff** watcher is
/// spawned so the session's terminal summary folds back into the parent conversation (S4/D2).
///
/// The spec's optional `profile` selects a `[[session_profiles]]` hat (S6): the pack that runs it,
/// the capability grant that bounds it, and the pack's opaque overrides.
pub async fn goals_start(
    State(state): State<Arc<AppState>>,
    Json(mut goal): Json<liberado_session::GoalSpec>,
) -> impl IntoResponse {
    let origin = goal.origin.clone();

    // Resolve the session's "hat" (S6) into a concrete authority before the kernel sees it. A
    // profile picks the pack, the capability grant, and the pack's opaque overrides; no profile
    // falls back to the bare domain, keyed by the domain name (the pool rule). Whether this session
    // may interrupt a human is decided *here*, by the grant â€” not by the caller asserting it.
    // A named profile that resolves to nothing is refused, not silently downgraded to the domain
    // grant — that would run the session with authority the caller never asked for.
    let resolved = match state
        .config
        .resolve_session_profile(goal.profile.as_deref(), goal.domain.as_str())
    {
        Ok(resolved) => resolved,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: e.to_string(),
                }),
            )
                .into_response();
        }
    };
    // A goal session needs a pack to run it, and a chat-only profile names none. Refused rather than
    // defaulted to the caller's domain: that would run the work under a pack the profile never
    // authorized, on nothing worse than picking the wrong name from a list.
    let Some(domain) = resolved.domain.as_deref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!(
                    "session profile '{}' declares no domain — it is a chat profile and cannot                      run a goal session",
                    resolved.name.as_deref().unwrap_or("?")
                ),
            }),
        )
            .into_response();
    };
    // A session that may do nothing is safe, and never useful. Refuse it here rather than start it.
    //
    // With no profile named, the grant is keyed by the **domain name** (the pool rule). A domain with
    // no matching `[[grants]]` entry therefore resolves to zero authority — correct as a fail-safe,
    // but the session then burns a run failing every action with a capability gap, and the error the
    // operator sees names the *MCP* rather than the missing grant.
    //
    // `resolve_session_profile` keeps its documented empty-on-unknown-domain behaviour; the refusal
    // belongs here, where "can do nothing" is actionable and can name both remedies.
    //
    // Ordered after the chat-profile check on purpose: that one establishes `domain`, and a chat
    // profile is the more specific diagnosis of the two.
    if goal.profile.is_none() && resolved.capabilities.capabilities.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!(
                    "domain '{domain}' has no capability grant, so this session could do nothing.                      Add a policy.toml [[grants]] entry with component = \"{domain}\", or name a                      session profile that carries one."
                ),
            }),
        )
            .into_response();
    }
    if domain != goal.domain.as_str() {
        goal.domain = liberado_session::DomainHint::from(domain);
    }
    // Per-goal idle wins; otherwise the profile default (E5 â€” hours for interactive coding).
    if goal.max_idle_secs.is_none() {
        goal.max_idle_secs = resolved.max_idle_secs;
    }
    let grant = liberado_session::SessionGrant {
        capabilities: resolved.capabilities,
        profile: goal.profile.clone(),
        overrides: serde_json::to_value(&resolved.overrides).unwrap_or(serde_json::Value::Null),
    };

    match state.goals.start_with_grant(goal, grant).await {
        Ok(id) => {
            if let Some(origin) = origin {
                spawn_return_handoff(state.clone(), id.clone(), origin);
            }
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "session_id": id, "status": "running" })),
            )
                .into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(ApiError { error: e })).into_response(),
    }
}

/// After an `origin`-linked session terminates, fold its summary back into the parent conversation
/// (session-focus S4 / D2). Spawned at start, it subscribes to the session's own event stream â€”
/// `subscribe()` returns catch-up history + a live receiver, so a finish between start and here is
/// not missed â€” waits for the terminal event, then appends a note via `ChatSessions::append_note`.
/// Best-effort: a missing conversation / disabled chat / append error is logged, never fatal.
fn spawn_return_handoff(
    state: Arc<AppState>,
    session_id: String,
    origin: liberado_session::SessionOrigin,
) {
    use liberado_session::SessionEventKind as K;
    tokio::spawn(async move {
        let Some((history, mut rx)) = state.goals.store().subscribe(&session_id).await else {
            return;
        };
        // A park is not a finish. `SessionFinished { status: "parked" }` closes the *stream*, but
        // the session is coming back — folding a summary into the parent now would announce an
        // ending that has not happened, and the terminal-settle wait below would spin for a second
        // and then log a spurious warning.
        let is_park = |e: &liberado_session::SessionEvent| matches!(&e.kind, K::SessionFinished { status, .. } if status == "parked");
        if history.iter().any(is_park) {
            return;
        }
        let already_done = history
            .iter()
            .any(|e| matches!(e.kind, K::SessionFinished { .. }));
        if !already_done {
            loop {
                match rx.recv().await {
                    Ok(ev) if is_park(&ev) => return,
                    Ok(ev) if matches!(ev.kind, K::SessionFinished { .. }) => break,
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }

        let Some(chat) = state.chat.as_ref() else {
            return; // no conversation store to fold into (chat disabled â€” there was no parent anyway)
        };
        // The `SessionFinished` *event* broadcasts just before `store.finish()` records the terminal
        // status/result, so read the snapshot only once the *record* has actually settled terminal â€”
        // otherwise the note would say "running" with no outcome.
        let mut snap = None;
        for _ in 0..200 {
            match state.goals.snapshot(&session_id).await {
                Some(s) if s.session.status.is_terminal() => {
                    snap = Some(s);
                    break;
                }
                Some(_) => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
                None => return,
            }
        }
        let Some(snap) = snap else {
            tracing::warn!(session = %session_id, "return handoff: record never settled terminal");
            return;
        };
        // No parent conversation â‡’ nothing to hand back to. That is the normal case for an
        // unattended session (a cron carries a correlation id but no parent), not an error.
        let Some(parent) = origin.conversation_id.as_deref() else {
            return;
        };
        let conv = match parent.parse::<Ulid>() {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(conversation = %parent, "return handoff: parent id is not a ULID");
                return;
            }
        };
        let note = format_handoff_note(&snap.session, &session_id);
        match chat.append_note(conv, note).await {
            Ok(()) => tracing::info!(
                session = %session_id,
                conversation = %parent,
                "return handoff: summary folded into parent conversation"
            ),
            Err(e) => tracing::warn!(error = %e, "return handoff: append_note failed"),
        }
    });
}

/// The note folded back into the parent conversation: session kind/id, terminal status, summary,
/// artifacts, and the `/join` hint so the human can reopen the full transcript. Compact by design â€”
/// the parent stays lean; the transcript lives in the session (D2).
fn format_handoff_note(record: &liberado_session::GoalSessionRecord, session_id: &str) -> String {
    let domain = record.goal.domain.as_str();
    let status = format!("{:?}", record.status).to_ascii_lowercase();
    let mut note = format!(
        "[{domain} session {status}] {}",
        record.goal.description.trim()
    );
    if let Some(result) = &record.result {
        if !result.summary.trim().is_empty() {
            note.push_str(&format!("\nOutcome: {}", result.summary.trim()));
        }
        if !result.artifacts.is_empty() {
            note.push_str(&format!("\nArtifacts: {}", result.artifacts.join(", ")));
        }
    }
    note.push_str(&format!(
        "\n(Rejoin the full transcript with /join {session_id}.)"
    ));
    note
}

/// `GET /api/goals/{id}` â€” session record + event history so far.
pub async fn goals_get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.goals.snapshot(&id).await {
        Some(snap) => Json(snap).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("goal session '{id}' not found"),
            }),
        )
            .into_response(),
    }
}

/// `POST /api/goals/{id}/cancel` â€” cooperative cancel of a running goal session.
pub async fn goals_cancel(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.goals.cancel(&id).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(ApiError { error: e })).into_response(),
    }
}

/// Body for [`goals_message`]: the human's reply to an interactive session's `AwaitingInput`.
#[derive(Deserialize)]
pub struct GoalMessageRequest {
    pub text: String,
}

/// `POST /api/goals/{id}/message` â€” deliver a human message into a running interactive goal
/// session (the reply to an `awaiting_input` prompt). A thin wrapper over
/// [`liberado_session::GoalSessionHub::send_input`], which echoes the message into the transcript
/// as a `human_input` event. `202 Accepted` on delivery; `404` when the session is unknown; `409`
/// when it has already finished (a terminal session accepts no input); `403` when the session's
/// grant omits `AskHuman` and so may *never* receive human input (S6) â€” an authority answer, not a
/// timing one.
/// `POST /api/goals/{id}/park` — ask a running session to wind down and land in `Parked`
/// (S2/G2). Cooperative, like cancel: it returns once the request is *accepted*, not once the pack
/// has stopped. Unlike cancel, the session keeps its awaiting-input state and can be resumed.
pub async fn goals_park(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.goals.park(&id).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "session_id": id, "status": "parking" })),
        )
            .into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(ApiError { error: e })).into_response(),
    }
}

pub async fn goals_message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<GoalMessageRequest>,
) -> impl IntoResponse {
    use liberado_session::SendInputError;
    // A parked session has no live pack to deliver into, so `send_input` cannot take the answer â€”
    // but for a pack that can rebuild itself from its transcript, answering IS the resume (E6-c).
    // Try the live door first (the overwhelmingly common case), and fall through to resume only for
    // the one error that means "parked". A pack that says it cannot be resumed still returns
    // `Parked`, and the caller still gets an honest 409 saying so.
    let outcome = match state.goals.send_input(&id, req.text.clone()).await {
        Err(SendInputError::Parked) => state.goals.resume(&id, req.text).await,
        other => other,
    };
    match outcome {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e @ SendInputError::Unknown) => (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
        Err(e @ SendInputError::NotPermitted) => (
            StatusCode::FORBIDDEN,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
        // `Parked` is a 409 like the others â€” the answer cannot be delivered â€” but its *message*
        // must not claim the session finished, because it has not. A client rendering "already
        // finished" over a session that is still holding a question for you is the difference
        // between "start over" and "wait".
        Err(e @ (SendInputError::Terminal | SendInputError::Parked | SendInputError::Closed)) => (
            StatusCode::CONFLICT,
            Json(ApiError {
                error: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// `GET /api/goals/{id}/stream` â€” SSE: catch-up history then live events.
/// Events use `event:` names matching [`liberado_session::SessionEventKind`] type tags
/// (`session_started`, `tool_started`, `session_finished`, â€¦); `data` is full JSON.
pub async fn goals_stream(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some((history, mut rx)) = state.goals.store().subscribe(&id).await else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiError {
                error: format!("goal session '{id}' not found"),
            }),
        )
            .into_response();
    };

    let stream = async_stream::stream! {
        for ev in history {
            yield Ok::<Event, Infallible>(session_event_to_sse(&ev));
        }
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let terminal = matches!(
                        ev.kind,
                        liberado_session::SessionEventKind::SessionFinished { .. }
                    );
                    yield Ok(session_event_to_sse(&ev));
                    if terminal {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(Box::pin(stream) as SseBody).into_response()
}

/// Encode a kernel [`liberado_session::SessionEvent`] as SSE. Event names are the kind's serde
/// tags â€” the same converged vocabulary `to_sse` maps chat's `AgentEvent` onto, decoded
/// client-side by `chat_client_contract::SessionEvent::from_sse_data`. `token` frames carry the
/// bare text (they're high-frequency); everything else carries the full event JSON.
fn session_event_to_sse(ev: &liberado_session::SessionEvent) -> Event {
    use liberado_session::SessionEventKind as K;
    if let K::Token { text } = &ev.kind {
        return Event::default().event("token").data(text.clone());
    }
    let name = match &ev.kind {
        K::SessionStarted { .. } => "session_started",
        K::RoleStarted { .. } => "role_started",
        K::RoleFinished { .. } => "role_finished",
        K::Token { .. } => unreachable!("handled above"),
        K::ToolStarted { .. } => "tool_started",
        K::ToolFinished { .. } => "tool_finished",
        K::Progress { .. } => "progress",
        K::AwaitingInput { .. } => "awaiting_input",
        K::HumanInput { .. } => "human_input",
        K::ValidationFinished { .. } => "validation_finished",
        K::CriticVerdict { .. } => "critic_verdict",
        K::FileChanged { .. } => "file_changed",
        K::LoopGuard { .. } => "loop_guard",
        K::SessionFinished { .. } => "session_finished",
        K::Failed { .. } => "failed",
    };
    let data = serde_json::to_string(ev).unwrap_or_else(|_| "{}".into());
    Event::default().event(name).data(data)
}

#[cfg(test)]
mod goal_message_tests {
    //! HTTP-level integration tests for `POST /api/goals/{id}/message`, against a real `axum::Router`
    //! wired to a `GoalSessionHub` with the life-ops demo pack (the same pattern as `hooks.rs`).

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
            vault_path: "/tmp/vault".to_string(),
            goals: goals.clone(),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
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
            vault_path: "/tmp/vault".to_string(),
            goals: goals.clone(),
            chat: Some(chat.clone()),
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
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
    /// it may write the note it was asked for. Interactivity is a capability (S6) â€” a session
    /// started without `AskHuman` cannot receive input at all, so these tests must grant it
    /// explicitly rather than relying on an ambient "interactive" payload flag.
    fn attended_life_grant() -> liberado_session::SessionGrant {
        liberado_session::SessionGrant {
            capabilities: life_capabilities(),
            profile: None,
            overrides: serde_json::Value::Null,
        }
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
            vault_path: "/tmp/vault".to_string(),
            goals: Arc::new(GoalSessionHub::new(GoalSessionStore::new())),
            chat: None,
            chat_tools: 0,
            chat_tool_names: Vec::new(),
            catalog: Arc::new(liberado_common::CapabilityCatalog::new()),
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
}
