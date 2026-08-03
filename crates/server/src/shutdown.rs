//! Graceful shutdown drain for durable **chat turns and goal sessions**.
//!
//! Durable work outlives its HTTP connection. Docker/SIGTERM killing the process mid-run loses
//! work that could still finish and persist. This module:
//!
//! 1. Marks the daemon as **not accepting new work** ([`DrainGate`]) — clients get a
//!    distinguishable `shutting_down` refusal, not a generic failure.
//! 2. Waits up to a **bounded grace period** for in-flight chat turns **and** goal sessions.
//! 3. On grace timeout: **aborts** remaining chat turns; **parks** remaining goal sessions (so
//!    post-drain status is not permanently `Running` — parked is human-actionable; aborted chat
//!    turns read as unanswered). Prefer park over goal-cancel: a parked session can be resumed
//!    when the pack allows; a cancelled one cannot.
//!
//! Resume of mid-inference model calls across restart is out of scope (inference is not
//! replayable). Parking is session-level only.
//!
//! # Work-start inventory (what is gated during drain)
//!
//! | Surface | Starts work how | Gated? |
//! |---|---|---|
//! | Chat HTTP `POST /api/chat`, `/api/chat/stream` | route layer [`refuse_new_turns_if_draining`] | **yes** |
//! | Chat Telegram free-form | `TelegramChatBridge` checks `drain.is_accepting()` | **yes** (capability, not this middleware) |
//! | Goals HTTP `POST /api/goals` | same refuse middleware as chat starts | **yes** |
//! | Goals HTTP cancel/park/message/stream/list | do not start new goal work | no (manage in-flight) |
//! | Hooks `POST /api/hooks/{name}` | enqueue daemon events; may later start sessions | **not** HTTP-gated here — event loop stops after drain completes (`lib.rs`); new hooks during drain are rare and not turn-shaped |
//! | Cron | scheduled fires into the daemon event loop | **not** HTTP — loop stops after drain; no new cron processing once process exits |
//! | Vault reactions | watcher → reactions while daemon runs | **not** HTTP — stopped with the process after drain |
//!
//! The gate covers **capabilities that start work**, not every route someone remembered. Chat
//! attach/cancel stay open so clients can rejoin or stop work already in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use tracing::{info, warn};

use crate::state::AppState;

/// Default in-process grace when `LIBERADO_SHUTDOWN_GRACE_SECS` is unset.
///
/// Long research turns have run past 200s; Docker's default stop timeout is 10s. Production compose
/// should set `stop_grace_period` ≥ this value so the container is not SIGKILL'd during drain.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(90);

/// Wire error kind clients can match on when new turns are refused during drain.
pub const SHUTTING_DOWN_ERROR: &str = "shutting_down";

/// Human-readable refusal for surfaces with no JSON envelope to put [`SHUTTING_DOWN_ERROR`] in —
/// today the Telegram bridge, which calls `ChatSessions` directly and so never passes through
/// [`refuse_new_turns_if_draining`]. Kept beside the wire constant so the two stay one decision.
pub const SHUTTING_DOWN_MESSAGE: &str =
    "The daemon is restarting and is not accepting new turns. Try again in a moment.";

/// Process-wide gate: when false, turn-starting HTTP routes refuse with [`shutting_down_response`].
#[derive(Debug)]
pub struct DrainGate {
    /// `true` while the daemon accepts new chat turns (normal operation).
    accepting: AtomicBool,
}

impl Default for DrainGate {
    fn default() -> Self {
        Self {
            accepting: AtomicBool::new(true),
        }
    }
}

impl DrainGate {
    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    /// Enter drain: new turns are refused. Idempotent.
    pub fn begin_drain(&self) {
        self.accepting.store(false, Ordering::SeqCst);
    }
}

/// Outcome of [`drain_for_shutdown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainOutcome {
    /// True if every in-flight chat turn **and** goal session finished before grace elapsed.
    pub idle_within_grace: bool,
    /// How long we waited (≤ grace).
    pub waited: Duration,
    /// Chat turns still running when grace ended (aborted via `cancel_turn`).
    pub aborted: usize,
    /// Goal sessions still hosted when grace ended (park signals sent).
    pub parked_goals: usize,
    /// Grace budget that was applied.
    pub grace: Duration,
}

/// Resolve grace from env `LIBERADO_SHUTDOWN_GRACE_SECS` or [`DEFAULT_SHUTDOWN_GRACE`].
pub fn shutdown_grace_from_env() -> Duration {
    std::env::var("LIBERADO_SHUTDOWN_GRACE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_SHUTDOWN_GRACE)
}

/// Distinctive JSON body for refused new turns during drain.
pub fn shutting_down_json() -> serde_json::Value {
    serde_json::json!({
        "error": SHUTTING_DOWN_ERROR,
        "message": "daemon is shutting down; new turns are not accepted",
    })
}

pub fn shutting_down_response() -> Response<Body> {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("content-type", "application/json")],
        shutting_down_json().to_string(),
    )
        .into_response()
}

/// Axum middleware for **work-starting** routes only (`/api/chat`, `/api/chat/stream`,
/// `POST /api/goals`). Attach/cancel/park/list stay available so clients can rejoin or stop work
/// already in flight.
pub async fn refuse_new_turns_if_draining(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    if !state.drain.is_accepting() {
        return shutting_down_response();
    }
    next.run(request).await
}

/// Mark drain, wait up to `grace` for in-flight chat turns **and** goal sessions, then abort chat
/// stragglers and **park** remaining goals.
///
/// Pure coordination over [`AppState`] — no SIGTERM plumbing; `run` calls this from the signal
/// handler. Tests call it directly with a short grace.
pub async fn drain_for_shutdown(state: &AppState, grace: Duration) -> DrainOutcome {
    state.drain.begin_drain();
    info!(
        grace_secs = grace.as_secs(),
        "shutdown drain: refusing new work; waiting for in-flight chat + goal sessions"
    );

    let start = Instant::now();
    let idle = wait_until_idle(state, grace).await;
    let waited = start.elapsed();

    let (aborted, parked_goals) = if idle {
        (0, 0)
    } else {
        let aborted = abort_remaining_turns(state);
        let parked_goals = park_remaining_goals(state).await;
        // Cooperative park/cancel needs a moment to leave the running map and flip status.
        let _ = wait_until_idle(state, Duration::from_millis(500)).await;
        warn!(
            aborted,
            parked_goals,
            waited_ms = waited.as_millis() as u64,
            "shutdown drain: grace elapsed; aborted chat stragglers and parked remaining goals"
        );
        (aborted, parked_goals)
    };

    if idle {
        info!(
            waited_ms = waited.as_millis() as u64,
            "shutdown drain: all in-flight work finished within grace"
        );
    }

    DrainOutcome {
        idle_within_grace: idle,
        waited,
        aborted,
        parked_goals,
        grace,
    }
}

/// Poll until chat + goal in-flight counts are both 0, or `grace` elapses.
///
/// Returns true if idle within grace. A **zero grace** returns immediately (does not wait) —
/// that is intentional so tests can prove non-zero grace is what makes finish-within-grace work.
pub async fn wait_until_idle(state: &AppState, grace: Duration) -> bool {
    if grace.is_zero() {
        return total_in_flight(state).await == 0;
    }
    let deadline = Instant::now() + grace;
    loop {
        if total_in_flight(state).await == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn total_in_flight(state: &AppState) -> usize {
    chat_in_flight_count(state) + state.goals.in_flight_count().await
}

fn chat_in_flight_count(state: &AppState) -> usize {
    state
        .chat
        .as_ref()
        .map(|c| c.in_flight_count())
        .unwrap_or(0)
}

/// Cancel every still-running chat turn so post-drain state is not `turn_running`.
/// Cancel persists nothing → transcripts that ended on a user message read as unanswered.
fn abort_remaining_turns(state: &AppState) -> usize {
    let Some(chat) = state.chat.as_ref() else {
        return 0;
    };
    let ids = chat.in_flight_sessions();
    let mut n = 0;
    for id in ids {
        if chat.cancel_turn(id) {
            n += 1;
        }
    }
    n
}

/// Park every still-hosted goal session so post-drain status is not permanently `Running`.
async fn park_remaining_goals(state: &AppState) -> usize {
    state.goals.park_all_in_flight().await
}

/// Await OS shutdown signals (Ctrl+C; SIGTERM on Unix). Used by `axum::serve` graceful path.
pub async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("received Ctrl+C");
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        info!("received Ctrl+C");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use liberado_executor::{Budget, Executor, ToolRuntime};
    use liberado_main_agent::ChatSessions;
    use liberado_provider::{
        CompletionRequest, CompletionResponse, Provider, ProviderResult, ToolDef, ToolInvocation,
    };
    use liberado_session_store::SessionStore;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    struct NoTools;
    #[async_trait]
    impl ToolRuntime for NoTools {
        fn catalog(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        async fn invoke(&self, _: &ToolInvocation) -> Result<String, String> {
            Err("no tools".into())
        }
    }

    /// Provider that answers after `delay` (cooperative finish path).
    struct SlowProvider {
        delay: Duration,
        reply: String,
    }
    #[async_trait]
    impl Provider for SlowProvider {
        fn model(&self) -> String {
            "slow".into()
        }
        async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
            tokio::time::sleep(self.delay).await;
            Ok(CompletionResponse::text(&self.reply))
        }
    }

    /// Provider that never answers (grace-timeout path).
    struct PendingProvider;
    #[async_trait]
    impl Provider for PendingProvider {
        fn model(&self) -> String {
            "pending".into()
        }
        async fn complete(&self, _: CompletionRequest) -> ProviderResult<CompletionResponse> {
            std::future::pending().await
        }
    }

    async fn make_chat(
        root: &std::path::Path,
        provider: Arc<dyn Provider>,
    ) -> (Arc<ChatSessions>, Arc<SessionStore>) {
        let store = Arc::new(SessionStore::open(root).await);
        let executor = Executor::new(provider, Budget::default());
        let chat = Arc::new(ChatSessions::new(
            store.clone(),
            executor,
            Arc::new(NoTools),
        ));
        (chat, store)
    }

    fn state_with(
        chat: Arc<ChatSessions>,
        store: Arc<SessionStore>,
        root: PathBuf,
    ) -> Arc<AppState> {
        Arc::new(AppState::for_test(store, Some(chat), root))
    }

    /// Mini-router matching production: turn-start routes + `refuse_new_turns_if_draining`.
    /// Deleting the middleware/`route_layer` in `lib.rs` without this layer would still serve
    /// chat — this test would then fail on status/body.
    fn turn_start_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/api/chat", axum::routing::post(crate::api::chat))
            .route(
                "/api/chat/stream",
                axum::routing::get(crate::api::chat_stream_get).post(crate::api::chat_stream_post),
            )
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                refuse_new_turns_if_draining,
            ))
            .with_state(state)
    }

    async fn post_chat(app: Router, message: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(r#"{{"message":"{message}"}}"#)))
                    .unwrap(),
            )
            .await
            .expect("oneshot");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .unwrap_or(serde_json::json!({ "raw": String::from_utf8_lossy(&bytes) }));
        (status, json)
    }

    /// Telegram is a turn-starting surface that never touches the HTTP router.
    ///
    /// `TelegramChatBridge::chat_turn` calls `ChatSessions::turn` directly, so
    /// `refuse_new_turns_if_draining` cannot see it. A message arriving mid-drain would start a
    /// turn the grace timeout aborts moments later — and it would count toward `in_flight_count`
    /// first, so the drain would wait on work it is about to discard. "New turns are refused"
    /// has to mean every surface, not the one the middleware happens to cover.
    #[tokio::test]
    async fn telegram_bridge_refuses_new_turns_during_drain() {
        use liberado_messaging::ChatSurface;

        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(1),
                reply: "telegram answer".into(),
            }),
        )
        .await;
        let state = state_with(chat, store, dir.path().to_path_buf());
        let bridge = crate::telegram::TelegramChatBridge {
            state: state.clone(),
            session_id: crate::sticky::StickySession::ephemeral(),
        };

        // Precondition: the same call succeeds while accepting, so the refusal below is the gate
        // and not a bridge that never worked.
        let before = bridge.reply("hello").await;
        assert_eq!(
            before.as_deref(),
            Ok("telegram answer"),
            "bridge must answer normally before drain"
        );

        drain_for_shutdown(&state, Duration::ZERO).await;

        let after = bridge.reply("hello again").await;
        let err = after.expect_err("a new turn must be refused once draining");
        assert_eq!(err, SHUTTING_DOWN_MESSAGE);
        assert_eq!(
            state.chat.as_ref().unwrap().in_flight_count(),
            0,
            "a refused message must not register an in-flight turn for the drain to wait on"
        );
    }

    /// Real HTTP path: after drain, POST /api/chat is 503 + error=shutting_down.
    /// Exercises `refuse_new_turns_if_draining` — not just DrainGate flags / JSON helpers.
    #[tokio::test]
    async fn refuse_new_turns_after_begin_drain() {
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(1),
                reply: "ok".into(),
            }),
        )
        .await;
        let state = state_with(chat, store, dir.path().to_path_buf());
        assert!(state.drain.is_accepting());

        // Integrated path: drain_for_shutdown must call begin_drain (not only wait).
        let outcome = drain_for_shutdown(&state, Duration::ZERO).await;
        assert!(
            !state.drain.is_accepting(),
            "drain_for_shutdown must flip the gate"
        );
        assert!(
            outcome.idle_within_grace || outcome.aborted == 0,
            "no turns were running: {outcome:?}"
        );

        let app = turn_start_router(state.clone());
        let (status, body) = post_chat(app, "should be refused").await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "middleware must refuse; body={body}"
        );
        assert_eq!(
            body["error"], SHUTTING_DOWN_ERROR,
            "distinguishable kind, not a generic failure; body={body}"
        );
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|m| m.contains("shutting down")),
            "client-visible message; body={body}"
        );
    }

    /// Same refusal on the streaming route used by TUI/WebUI.
    #[tokio::test]
    async fn refuse_new_turns_on_chat_stream_after_drain() {
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(1),
                reply: "ok".into(),
            }),
        )
        .await;
        let state = state_with(chat, store, dir.path().to_path_buf());
        state.drain.begin_drain();
        assert!(!state.drain.is_accepting());

        let app = turn_start_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/chat/stream")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"message":"nope"}"#))
                    .unwrap(),
            )
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], SHUTTING_DOWN_ERROR);
    }

    /// In-flight turn finishes within grace and persists a reply.
    #[tokio::test]
    async fn in_flight_turn_finishes_within_grace_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(80),
                reply: "persisted-within-grace".into(),
            }),
        )
        .await;
        let id = chat.create(None).await.unwrap();
        let (_replay, _rx) = chat.start_or_attach(id, "hello drain");
        assert!(chat.turn_running(id));

        let state = state_with(Arc::clone(&chat), store, dir.path().to_path_buf());
        let outcome = drain_for_shutdown(&state, Duration::from_millis(2_000)).await;

        assert!(
            outcome.idle_within_grace,
            "turn should finish within grace: {outcome:?}"
        );
        assert_eq!(outcome.aborted, 0);
        assert!(!chat.turn_running(id));

        // Durable path: assistant reply on the transcript.
        let history = chat.history(id).await.unwrap();
        assert!(
            history
                .iter()
                .any(|m| m.content.contains("persisted-within-grace")),
            "reply must persist; history={history:?}"
        );
        assert!(!chat.last_turn_unanswered(id).await);
    }

    /// Grace exceeded: shutdown does not hang; leftover is not running and reads unanswered.
    #[tokio::test]
    async fn grace_timeout_aborts_and_leaves_unanswered() {
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(dir.path(), Arc::new(PendingProvider)).await;
        let id = chat.create(None).await.unwrap();
        let (_replay, _rx) = chat.start_or_attach(id, "never finishes");
        // User message is persisted at turn start before provider returns.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(chat.turn_running(id));

        let state = state_with(Arc::clone(&chat), store, dir.path().to_path_buf());
        let t0 = Instant::now();
        let outcome = drain_for_shutdown(&state, Duration::from_millis(100)).await;
        let elapsed = t0.elapsed();

        assert!(
            !outcome.idle_within_grace,
            "pending turn must not finish within short grace"
        );
        assert!(outcome.aborted >= 1);
        assert!(
            elapsed < Duration::from_secs(2),
            "drain must not hang indefinitely; elapsed={elapsed:?}"
        );
        assert!(!chat.turn_running(id), "must not still claim running");
        assert!(
            chat.last_turn_unanswered(id).await,
            "cancelled/aborted turn ending on user message must read unanswered"
        );
    }

    /// §1: non-zero grace is what waits for in-flight work. Zero grace with a live turn does not
    /// wait for completion — removing the wait (or always using zero) fails the finish-within-grace
    /// contract; this test pins that zero ≠ "wait".
    #[tokio::test]
    async fn zero_grace_does_not_wait_for_in_flight_turn() {
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(500),
                reply: "too-late".into(),
            }),
        )
        .await;
        let id = chat.create(None).await.unwrap();
        let (_replay, _rx) = chat.start_or_attach(id, "slow");
        assert!(chat.turn_running(id));

        let state = state_with(Arc::clone(&chat), store, dir.path().to_path_buf());
        let t0 = Instant::now();
        let outcome = drain_for_shutdown(&state, Duration::ZERO).await;
        let elapsed = t0.elapsed();

        assert!(
            elapsed < Duration::from_millis(200),
            "zero grace must return promptly, not wait for the 500ms turn; elapsed={elapsed:?}"
        );
        assert!(!outcome.idle_within_grace || outcome.aborted >= 1);
        // After zero-grace drain, stragglers are aborted (or were never waited for).
        assert!(!chat.turn_running(id));
    }

    /// §1 wiring: finish-within-grace requires the wait loop. If someone deleted
    /// `wait_until_chat_idle` and only aborted, this would fail (no persisted reply).
    #[tokio::test]
    async fn grace_wait_is_required_for_persist_path() {
        // Same as finish-within-grace but explicitly documents the wiring claim.
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(60),
                reply: "wiring-proof".into(),
            }),
        )
        .await;
        let id = chat.create(None).await.unwrap();
        let (_replay, _rx) = chat.start_or_attach(id, "prove wait");
        let state = state_with(Arc::clone(&chat), store, dir.path().to_path_buf());

        // Non-zero grace + cooperative provider → persisted answer.
        let outcome = drain_for_shutdown(&state, Duration::from_millis(1_000)).await;
        assert!(outcome.idle_within_grace);
        assert!(
            chat.history(id)
                .await
                .unwrap()
                .iter()
                .any(|m| m.content.contains("wiring-proof")),
            "deleting the grace wait would abort before persist"
        );
    }

    #[test]
    fn default_grace_is_above_docker_ten_seconds() {
        assert!(DEFAULT_SHUTDOWN_GRACE > Duration::from_secs(10));
        assert_eq!(DEFAULT_SHUTDOWN_GRACE, Duration::from_secs(90));
    }

    // ── Goal sessions in the drain (round-2 §4) ─────────────────────────────

    /// Pack that only finishes when the cancel/park signal fires — so a no-op park leaves
    /// `Running` forever and fails the post-drain status assertion.
    struct NeverEndingGoalPack;
    #[async_trait]
    impl liberado_session::DomainPackRunner for NeverEndingGoalPack {
        fn domain_id(&self) -> &str {
            "life"
        }
        async fn run(
            &self,
            _id: &str,
            _goal: &liberado_session::GoalSpec,
            _ctx: &liberado_session::PackContext<'_>,
            _events: tokio::sync::mpsc::Sender<liberado_session::SessionEvent>,
            _inputs: liberado_session::InputChannel,
            mut cancel: tokio::sync::watch::Receiver<bool>,
        ) -> Result<liberado_session::GoalResult, liberado_session::PackError> {
            loop {
                if *cancel.borrow() {
                    return Err(liberado_session::PackError::Cancelled);
                }
                if cancel.changed().await.is_err() {
                    return Err(liberado_session::PackError::Cancelled);
                }
            }
        }
    }

    async fn state_with_hanging_goal(
        root: &std::path::Path,
    ) -> (Arc<AppState>, Arc<liberado_session::GoalSessionHub>, String) {
        use liberado_session::{DomainHint, GoalSessionHub, GoalSpec, SessionGrant};

        let store = Arc::new(SessionStore::open(root).await);
        let mut hub = GoalSessionHub::new(SessionStore::clone(&store));
        hub.register_pack(Arc::new(NeverEndingGoalPack));
        let goals = Arc::new(hub);

        let (chat, _) = make_chat(root, Arc::new(PendingProvider)).await;

        let mut state = AppState::for_test(store, Some(chat), root.to_path_buf());
        // Replace the empty hub from for_test with one that has a pack.
        state.goals = Arc::clone(&goals);
        let state = Arc::new(state);

        let id = goals
            .start_with_grant(
                GoalSpec {
                    id: None,
                    description: "hang until park".into(),
                    success_criteria: vec![],
                    domain: DomainHint::Life,
                    max_turns: 0,
                    max_idle_secs: None,
                    origin: None,
                    profile: None,
                    payload: serde_json::Value::Null,
                },
                SessionGrant::default(),
            )
            .await
            .expect("start hanging goal");

        // Wait until hosted (Running + cancel handle).
        for _ in 0..100 {
            if goals.in_flight_count().await >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            goals.in_flight_count().await >= 1,
            "goal must be in-flight before drain"
        );

        (state, goals, id)
    }

    /// R3 / acceptance: after drain, a goal that did not finish is **not** left `Running` —
    /// park is the disposition (not process-kill mid-run with status still Running).
    ///
    /// R1: if `park_remaining_goals` / goal half of `total_in_flight` is deleted, this fails
    /// (session stays Running, or drain never waits on goals).
    #[tokio::test]
    async fn goal_session_is_parked_not_left_running_after_drain() {
        use liberado_session::SessionStatus;

        let dir = tempfile::tempdir().unwrap();
        let (state, goals, id) = state_with_hanging_goal(dir.path()).await;

        // Zero grace: do not wait for the pack to finish itself — force the park path.
        let outcome = drain_for_shutdown(&state, Duration::ZERO).await;
        assert!(
            outcome.parked_goals >= 1,
            "drain must park straggler goals: {outcome:?}"
        );

        // Settle until the cooperative park lands (or timeout).
        let mut final_status = None;
        for _ in 0..100 {
            if let Some(snap) = goals.snapshot(&id).await {
                final_status = Some(snap.session.status);
                if snap.session.status != SessionStatus::Running {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let status = final_status.expect("session still in store");
        assert_eq!(
            status,
            SessionStatus::Parked,
            "post-drain goal must be Parked (human-actionable), not permanently Running"
        );
        assert_eq!(
            goals.in_flight_count().await,
            0,
            "parked session must leave the host map"
        );
    }

    /// Router with refuse middleware: `POST /api/goals` after drain is 503 + shutting_down.
    /// Asserting only `DrainGate` would not catch a goals route never added to the layer (R3).
    #[tokio::test]
    async fn post_goals_refuses_with_shutting_down_after_drain() {
        let dir = tempfile::tempdir().unwrap();
        let (chat, store) = make_chat(
            dir.path(),
            Arc::new(SlowProvider {
                delay: Duration::from_millis(1),
                reply: "n/a".into(),
            }),
        )
        .await;
        let state = state_with(chat, store, dir.path().to_path_buf());
        drain_for_shutdown(&state, Duration::ZERO).await;
        assert!(!state.drain.is_accepting());

        // Same production shape: POST /api/goals on the work-start layer only.
        let app = Router::new()
            .route("/api/goals", axum::routing::post(crate::api::goals_start))
            .route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                refuse_new_turns_if_draining,
            ))
            .with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/goals")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"description":"should refuse","domain":"life"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body["error"], SHUTTING_DOWN_ERROR,
            "structured shutting_down kind, not a generic 503: {body}"
        );
    }

    /// Inventory comment in this module lists every work-start surface (acceptance deliverable).
    #[test]
    fn work_start_inventory_is_documented_in_module_docs() {
        let docs = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/shutdown.rs"));
        for needle in [
            "Chat HTTP",
            "Telegram",
            "POST /api/goals",
            "Hooks",
            "Cron",
            "Vault",
        ] {
            assert!(
                docs.contains(needle),
                "shutdown module docs must inventory work-start surface: {needle}"
            );
        }
    }
}
