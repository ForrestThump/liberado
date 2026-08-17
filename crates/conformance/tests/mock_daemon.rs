//! Conformance paths exercised against a **mock daemon** (in-process axum server) — no live box,
//! no network, deterministic on CI.
//!
//! The mock serves the daemon's public API surface the paths depend on (`/api/status`,
//! `/api/reactions`, `/api/hooks/…`, `/api/goals/…`, `/api/chat/stream`, `/api/conversations/…`,
//! `/api/sessions`, `/api/chat`, `/api/conversations/{id}/cancel`) with a small state machine:
//! chat-stream prompts route to the behaviour the path under test expects (P2 pong, P6 durable /
//! cancel, P7 restart, P5 delegate child), and hooks write the P1b vault artifact the path reads
//! back (the run_id arrives via the `X-Liberado-Idempotency-Key` header, exactly as on a real box).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::{Json, Path as AxumPath, Request, State as AxumState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chat_client_contract::{DaemonStatus, ReactionEvent, ReactionOutcome};
use liberado_conformance::client::DaemonClient;
use liberado_conformance::config::ConformanceConfig;
use liberado_conformance::paths::run_path;
use liberado_conformance::result::{PathId, PathResult, PathStatus};
use serde_json::{Value, json};

// ── Mock daemon ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// Finishes immediately with a reply (P2, P5).
    FinishedNow,
    /// turn_running until attached, then finishes with a reply (P6 outlive arm).
    Outlive,
    /// turn_running until cancelled, then leaves the question only (P6 cancel arm).
    CancelMe,
    /// turn_running until the mock serves the drain 503, then honestly unanswered (P7).
    Restart,
}

#[derive(Clone, Debug)]
struct Conversation {
    kind: Kind,
    user_content: String,
    turn_running: bool,
    turn_unanswered: bool,
    assistant: Option<String>,
    model: Option<String>,
    attached: bool,
    cancelled: bool,
}

/// Test-controllable daemon state. Defaults model a healthy daemon; each test flips only what it
/// needs to force the path under test down its branch.
#[derive(Clone)]
struct State {
    reactions: Vec<ReactionEvent>,
    conversations: HashMap<String, Conversation>,
    sessions: Vec<Value>,
    goals: HashMap<String, Value>,
    uptime_seconds: u64,
    model_name: Option<String>,
    /// Vault root the mock writes P1b artifacts under (like the box's dispatched turn would).
    vault_path: Option<PathBuf>,
    shutting_down: bool,
    /// Session visibility the mock stamps on new background sessions ("background" default).
    sessions_visibility: Option<String>,
    /// Emit a token event on chat/stream when true.
    chat_stream_token: bool,
    /// Model stamped on assistant messages (defaults to `model_name`).
    assistant_model_override: Option<String>,
    /// Body POST /api/goals returns.
    goals_post_body: Value,
    /// Add the P5 delegate child session when the prompt asks to delegate.
    delegate_child: bool,
    /// Attach serves session framing only (no token) — forces the P6 no-content branch.
    attach_without_token: bool,
    /// Goal body the hook writes for its dispatched session (default succeeded).
    hook_goal_body: Value,
    /// HTTP status /api/chat/stream serves (default 200) — forces client error branches.
    chat_stream_status: u16,
    /// When > 0, the P5 delegate child appears only after this many /api/sessions polls.
    delegate_child_delay_polls: u64,
    sessions_calls: u64,
    pending_child: Option<Value>,
    /// chat/stream body ends without the final blank line — exercises the client's trailing-block
    /// parse (a stream can end mid-frame).
    sse_without_trailing_blank: bool,
    /// conversation GET serves a user-only transcript (no assistant node).
    conversation_user_only: bool,
    next_id: u64,
}

impl State {
    fn healthy() -> Self {
        Self {
            reactions: vec![],
            conversations: HashMap::new(),
            sessions: vec![],
            goals: HashMap::new(),
            uptime_seconds: 200_000,
            model_name: Some("mock-model".into()),
            vault_path: None,
            shutting_down: false,
            sessions_visibility: None,
            chat_stream_token: true,
            assistant_model_override: None,
            goals_post_body: json!({}),
            delegate_child: true,
            attach_without_token: false,
            hook_goal_body: json!({"status": "succeeded"}),
            chat_stream_status: 200,
            delegate_child_delay_polls: 0,
            sessions_calls: 0,
            pending_child: None,
            sse_without_trailing_blank: false,
            conversation_user_only: false,
            next_id: 0,
        }
    }

    fn next(&mut self) -> String {
        self.next_id += 1;
        format!("sess-{}", self.next_id)
    }
}

type Shared = Arc<Mutex<State>>;

fn sse_response(body: &str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn router(state: Shared) -> Router {
    Router::new()
        .route("/api/status", get(status_h))
        .route("/api/reactions", get(reactions_h))
        .route("/api/goals", post(goals_post_h))
        .route("/api/goals/{id}", get(goals_get_h))
        .route("/api/goals/{id}/stream", get(goal_stream_h))
        .route("/api/hooks/{name}", post(hook_h))
        .route("/api/chat/stream", post(chat_stream_h))
        .route("/api/chat", post(chat_post_h))
        .route("/api/conversations/{id}", get(conversation_h))
        .route("/api/conversations/{id}/attach", get(attach_h))
        .route("/api/conversations/{id}/cancel", post(cancel_h))
        .route("/api/sessions", get(sessions_h))
        .with_state(state)
}

async fn status_h(AxumState(s): AxumState<Shared>) -> Json<DaemonStatus> {
    let s = s.lock().unwrap();
    Json(DaemonStatus {
        running: true,
        vault_path: s
            .vault_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        uptime_seconds: s.uptime_seconds,
        watcher_active: false,
        dispatcher_attached: false,
        orchestrator_attached: false,
        reactions_seen: s.reactions.len() as u64,
        model_name: s.model_name.clone(),
        token_usage_total: None,
        context_window: None,
        chat_tools: 0,
        chat_tool_names: vec![],
        enter_sends: true,
    })
}

async fn reactions_h(AxumState(s): AxumState<Shared>) -> Json<Vec<ReactionEvent>> {
    Json(s.lock().unwrap().reactions.clone())
}

async fn goals_get_h(AxumState(s): AxumState<Shared>, AxumPath(id): AxumPath<String>) -> Response {
    let s = s.lock().unwrap();
    let body = s
        .goals
        .get(&id)
        .cloned()
        .unwrap_or_else(|| json!({"status": "succeeded"}));
    Json(body).into_response()
}

async fn goal_stream_h(
    AxumState(s): AxumState<Shared>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    // One chunk immediately — `goal_stream_accepts` reads a single chunk under a 5s timeout.
    let _ = s;
    sse_response(&format!(
        "event: session\ndata: {{\"session\":\"{id}\"}}\n\n"
    ))
}

async fn goals_post_h(AxumState(s): AxumState<Shared>, Json(_body): Json<Value>) -> Json<Value> {
    let mut s = s.lock().unwrap();
    let body = s.goals_post_body.clone();
    // If the test's body carries a session_id, that's the goal GET must serve later (P4 grant).
    if let Some(sid) = body.get("session_id").and_then(|v| v.as_str()) {
        s.goals.insert(sid.to_string(), body.clone());
    }
    Json(body)
}

async fn hook_h(
    AxumState(s): AxumState<Shared>,
    AxumPath(name): AxumPath<String>,
    req: Request,
) -> Response {
    let run_id = req
        .headers()
        .get("x-liberado-idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let mut s = s.lock().unwrap();
    let corr = format!("hook-{}", s.next());
    let session = s.next();

    // The dispatched session's ground truth: a vault artifact containing CONFORMANCE_OK <run_id>.
    if let Some(vault) = &s.vault_path {
        let dir = vault.join("conformance").join("artifacts");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join(format!("{run_id}.md")),
            format!("CONFORMANCE_OK {run_id}"),
        );
    }

    let goal = s.hook_goal_body.clone();
    s.goals.insert(session.clone(), goal);
    s.reactions.push(ReactionEvent {
        event_type: "WebhookFired".into(),
        timestamp: now_rfc3339(),
        source: format!("webhook:{name}"),
        correlation_id: corr.clone(),
        path: None,
        outcome: ReactionOutcome::Dispatched {
            session_id: session.clone(),
        },
    });
    Json(json!({"correlation_id": corr, "session_id": session})).into_response()
}

async fn chat_stream_h(AxumState(s): AxumState<Shared>, Json(body): Json<Value>) -> Response {
    let message = body
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let background = body
        .get("background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut s = s.lock().unwrap();
    if s.chat_stream_status != 200 {
        return (
            StatusCode::from_u16(s.chat_stream_status).unwrap(),
            "rejected",
        )
            .into_response();
    }
    let id = s.next();

    let kind = if message.contains("pong") || message.contains("delegate") {
        Kind::FinishedNow
    } else if message.contains("five short paragraphs") {
        Kind::Outlive
    } else if message.contains("numbered list") {
        Kind::CancelMe
    } else if message.contains("graceful shutdown") {
        Kind::Restart
    } else {
        Kind::FinishedNow
    };
    let emit_token = s.chat_stream_token;
    let no_trailing_blank = s.sse_without_trailing_blank;

    let assistant = if kind == Kind::FinishedNow {
        Some("pong".to_string())
    } else {
        None
    };

    let model = s
        .assistant_model_override
        .clone()
        .or_else(|| s.model_name.clone());
    s.conversations.insert(
        id.clone(),
        Conversation {
            kind: kind.clone(),
            user_content: message.clone(),
            turn_running: kind != Kind::FinishedNow,
            turn_unanswered: false,
            assistant,
            model,
            attached: false,
            cancelled: false,
        },
    );

    if background {
        let vis = s
            .sessions_visibility
            .clone()
            .unwrap_or_else(|| "background".into());
        s.sessions
            .push(json!({"id": id.clone(), "visibility": vis}));
        if message.contains("delegate") {
            let child = s.next();
            let child_entry = json!({
                "id": child,
                "visibility": "background",
                "parent_session": id,
            });
            if s.delegate_child_delay_polls > 0 {
                // The child "spawns" asynchronously on the real box — appear later.
                s.pending_child = Some(child_entry);
            } else if s.delegate_child {
                s.sessions.push(child_entry);
            }
        }
    }
    drop(s);

    let mut sse = format!("event: session\ndata: {{\"session\":\"{id}\"}}\n\n");
    if kind == Kind::FinishedNow && emit_token {
        sse.push_str("event: token\ndata: pong\n\n");
    }
    if no_trailing_blank {
        // Cut the final blank line: the stream ends mid-frame.
        sse.pop();
        sse.pop();
    }
    sse_response(&sse)
}

async fn chat_post_h(AxumState(s): AxumState<Shared>, Json(_body): Json<Value>) -> Response {
    let mut s = s.lock().unwrap();
    if s.shutting_down {
        // The restart "killed" the process: honest lifecycle flags on any restart-kind turn.
        for c in s.conversations.values_mut() {
            if c.kind == Kind::Restart {
                c.turn_running = false;
                c.turn_unanswered = true;
            }
        }
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "shutting_down"})),
        )
            .into_response();
    }
    Json(json!({"ok": true})).into_response()
}

async fn conversation_h(
    AxumState(s): AxumState<Shared>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let mut s = s.lock().unwrap();
    let user_only = s.conversation_user_only;
    let Some(c) = s.conversations.get_mut(&id) else {
        return (StatusCode::NOT_FOUND, "no such conversation").into_response();
    };
    if c.kind == Kind::Outlive && c.attached {
        c.turn_running = false;
        c.assistant.get_or_insert_with(|| "durable answer".into());
    }
    if c.cancelled {
        c.turn_running = false;
        c.assistant = None;
    }

    let mut messages = vec![json!({
        "role": "user",
        "content": c.user_content,
        "model": c.model,
    })];
    if !user_only && let Some(a) = &c.assistant {
        messages.push(json!({"role": "assistant", "content": a, "model": c.model}));
    }
    Json(json!({
        "turn_running": c.turn_running,
        "turn_unanswered": c.turn_unanswered,
        "messages": messages,
    }))
    .into_response()
}

async fn attach_h(AxumState(s): AxumState<Shared>, AxumPath(id): AxumPath<String>) -> Response {
    let mut s = s.lock().unwrap();
    let no_token = s.attach_without_token;
    let Some(c) = s.conversations.get_mut(&id) else {
        return (StatusCode::NOT_FOUND, "no such conversation").into_response();
    };
    let mut sse = format!("event: session\ndata: {{\"session\":\"{id}\"}}\n\n");
    if !no_token && (c.assistant.is_some() || c.kind == Kind::Outlive) {
        sse.push_str("event: token\ndata: durable answer\n\n");
    }
    c.attached = true;
    if c.kind == Kind::Outlive {
        c.assistant.get_or_insert_with(|| "durable answer".into());
        c.turn_running = false;
    }
    sse_response(&sse)
}

async fn cancel_h(AxumState(s): AxumState<Shared>, AxumPath(id): AxumPath<String>) -> Response {
    let mut s = s.lock().unwrap();
    let Some(c) = s.conversations.get_mut(&id) else {
        return (StatusCode::NOT_FOUND, "no such conversation").into_response();
    };
    c.cancelled = true;
    c.turn_running = false;
    c.turn_unanswered = true;
    c.assistant = None;
    (StatusCode::OK, "cancelled").into_response()
}

async fn sessions_h(AxumState(s): AxumState<Shared>) -> Json<Value> {
    let mut s = s.lock().unwrap();
    s.sessions_calls += 1;
    if s.sessions_calls >= s.delegate_child_delay_polls
        && let Some(child) = s.pending_child.take()
    {
        s.sessions.push(child);
    }
    Json(json!(s.sessions.clone()))
}

/// Bind the mock, return the client pointing at it plus the base URL.
async fn spawn(client_state: State) -> (DaemonClient, String) {
    let app = router(Arc::new(Mutex::new(client_state)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}");
    (DaemonClient::new(&base).unwrap(), base)
}

// ── Test helpers ──────────────────────────────────────────────────────────────

fn base_cfg(base_url: &str, vault: &Path) -> ConformanceConfig {
    ConformanceConfig {
        base_url: base_url.to_string(),
        budget_secs: 60,
        vault_path: vault.to_path_buf(),
        topology_path: None,
        hook_name: "conformance".into(),
        hook_secret_ref: "UNSET_CONFORMANCE_TEST_SECRET".into(),
        profile_name: "conformance".into(),
        paths: vec![],
        advisory_counts: false,
        path_timeout_secs: 10,
        restart_command: None,
    }
}

async fn run(client: &DaemonClient, cfg: &ConformanceConfig, id: PathId) -> PathResult {
    let deadline = Instant::now() + cfg.budget();
    run_path(id, client, cfg, deadline).await
}

fn assert_status(r: &PathResult, want: PathStatus) {
    assert_eq!(
        r.status, want,
        "{}: {} {:?} — reason={:?} evidence={:?}",
        r.path, r.assertion, r.status, r.reason, r.evidence
    );
}

fn write_topology(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("topology.toml");
    std::fs::write(&p, body).unwrap();
    p
}

fn reaction(source: &str, timestamp: &str) -> ReactionEvent {
    ReactionEvent {
        event_type: "WebhookFired".into(),
        timestamp: timestamp.into(),
        source: source.into(),
        correlation_id: "x".into(),
        path: None,
        outcome: ReactionOutcome::Observed,
    }
}

fn cfg_with_secret(base: &str, vault: &Path, var_name: &str, secret: &str) -> ConformanceConfig {
    unsafe {
        std::env::set_var(var_name, secret);
    }
    ConformanceConfig {
        hook_secret_ref: var_name.to_string(),
        ..base_cfg(base, vault)
    }
}

// ── P1a: cron liveness ────────────────────────────────────────────────────────

const TOPOLOGY: &str = r#"
[[schedules]]
name = "daily-planning"
enabled = true
cron_expr = "0 55 11 * * * *"

[[schedules]]
name = "conformance"
enabled = true
cron_expr = "0 0 * * * * *"
"#;

#[tokio::test]
async fn p1a_passes_when_schedules_have_recent_reactions() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state
        .reactions
        .push(reaction("cron:daily-planning", &now_rfc3339()));
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(dir.path(), TOPOLOGY));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Pass);
}

#[tokio::test]
async fn p1a_fails_on_stale_reaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    // 3 days old — beyond the 1.5× period gate for a daily schedule.
    let stale = (chrono::Utc::now() - chrono::Duration::days(3)).to_rfc3339();
    state
        .reactions
        .push(reaction("cron:daily-planning", &stale));
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(dir.path(), TOPOLOGY));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Fail);
    assert!(r.reason.is_none());
    let ev = r.evidence.unwrap();
    assert_eq!(ev["failures"][0]["status"], "stale");
}

#[tokio::test]
async fn p1a_fails_on_missing_reaction() {
    let dir = tempfile::tempdir().unwrap();
    let state = State::healthy(); // no reactions at all
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(dir.path(), TOPOLOGY));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Fail);
    assert_eq!(r.evidence.unwrap()["failures"][0]["status"], "missing");
}

#[tokio::test]
async fn p1a_skips_when_uptime_below_period_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.uptime_seconds = 60; // restart gate: below 1.5× the daily period
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(dir.path(), TOPOLOGY));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Skipped);
    assert!(r.reason.unwrap().contains("restart gate"));
}

#[tokio::test]
async fn p1a_skips_without_topology_path() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = base_cfg(&base, dir.path()); // topology_path None

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Skipped);
}

#[tokio::test]
async fn p1a_skips_when_only_suite_owned_schedules() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(
        dir.path(),
        r#"
[[schedules]]
name = "conformance"
enabled = true
cron_expr = "0 0 * * * * *"

[[schedules]]
name = "conformance-notify"
enabled = true
cron_expr = "0 0 * * * * *"
"#,
    ));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Skipped);
    assert!(r.reason.unwrap().contains("no enabled non-suite"));
}

/// Hourly schedule: period 3600s, freshness threshold 1.5× = 5400s.
const HOURLY_TOPOLOGY: &str = r#"
[[schedules]]
name = "hourly-check"
enabled = true
cron_expr = "0 0 * * * * *"
"#;

/// A reaction 5,000s old is inside the 5,400s gate but outside any `+`/`/` corruption of the
/// 1.5× multiplier — pins the threshold arithmetic, not just "fresh vs very old".
#[tokio::test]
async fn p1a_passes_when_reaction_within_period_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    let age = chrono::Duration::seconds(5_000);
    state.reactions.push(reaction(
        "cron:hourly-check",
        &(chrono::Utc::now() - age).to_rfc3339(),
    ));
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(dir.path(), HOURLY_TOPOLOGY));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Pass);
}

/// Uptime exactly AT the 1.5× period must NOT trip the restart gate (`<`, not `<=`).
#[tokio::test]
async fn p1a_exact_threshold_uptime_does_not_skip() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.uptime_seconds = 5_400; // exactly the hourly threshold
    state
        .reactions
        .push(reaction("cron:hourly-check", &now_rfc3339()));
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.topology_path = Some(write_topology(dir.path(), HOURLY_TOPOLOGY));

    let r = run(&client, &cfg, PathId::P1a).await;
    assert_status(&r, PathStatus::Pass);
}

// ── P1b: hook → dispatch → execute → artifact ────────────────────────────────

#[tokio::test]
async fn p1b_passes_full_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.vault_path = Some(dir.path().to_path_buf()); // mock writes the artifact
    let (client, base) = spawn(state).await;
    let cfg = cfg_with_secret(&base, dir.path(), "CONF_P1B_OK", "s3cr3t");

    let r = run(&client, &cfg, PathId::P1b).await;
    assert_status(&r, PathStatus::Pass);
}

#[tokio::test]
async fn p1b_fails_when_artifact_missing() {
    let dir = tempfile::tempdir().unwrap();
    let state = State::healthy(); // vault_path None → mock writes no artifact
    let (client, base) = spawn(state).await;
    let cfg = cfg_with_secret(&base, dir.path(), "CONF_P1B_MISSING", "s3cr3t");

    let r = run(&client, &cfg, PathId::P1b).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("artifact exists on disk"),
        "{}",
        r.assertion
    );
}

#[tokio::test]
async fn p1b_fails_when_terminal_not_succeeded() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.vault_path = Some(dir.path().to_path_buf()); // mock writes the artifact
    state.hook_goal_body = json!({"status": "failed"}); // terminal but not succeeded
    let (client, base) = spawn(state).await;
    let cfg = cfg_with_secret(&base, dir.path(), "CONF_P1B_TERMINAL", "s3cr3t");
    let r = run(&client, &cfg, PathId::P1b).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("terminal status is succeeded"),
        "{}",
        r.assertion
    );
}

// ── P3: hook → joinable session ───────────────────────────────────────────────

#[tokio::test]
async fn p3_passes_when_session_joinable() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = cfg_with_secret(&base, dir.path(), "CONF_P3_OK", "s3cr3t");

    let r = run(&client, &cfg, PathId::P3).await;
    assert_status(&r, PathStatus::Pass);
}

#[tokio::test]
async fn p3_skips_without_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = base_cfg(&base, dir.path()); // secret ref unset

    let r = run(&client, &cfg, PathId::P3).await;
    assert_status(&r, PathStatus::Skipped);
    assert!(r.reason.unwrap().contains("unset"));
}

// ── P2: background chat turn ──────────────────────────────────────────────────

#[tokio::test]
async fn p2_passes_with_tokens_transcript_and_model_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Pass);
}

#[tokio::test]
async fn p2_fails_when_visibility_not_background() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.sessions_visibility = Some("foreground".into());
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("visibility is background"),
        "{}",
        r.assertion
    );
}

#[tokio::test]
async fn p2_fails_without_token() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.chat_stream_token = false;
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("at least one Token"),
        "{}",
        r.assertion
    );
}

#[tokio::test]
async fn p2_fails_on_model_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.assistant_model_override = Some("other-model".into());
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("model equals daemon active"),
        "{}",
        r.assertion
    );
}

#[tokio::test]
async fn p2_fails_when_chat_stream_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.chat_stream_status = 500;
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("POST /api/chat/stream"),
        "{}",
        r.assertion
    );
    // Distinguishes the non-success branch from the never-announced-session branch.
    assert!(
        r.evidence
            .as_ref()
            .and_then(|e| e.get("error"))
            .and_then(|e| e.as_str())
            .is_some_and(|s| s.contains("chat/stream 500")),
        "evidence: {:?}",
        r.evidence
    );
}

/// A stream that ends mid-frame (no final blank line) must still parse its trailing block — the
/// client's `if !buffer.trim().is_empty()` trailing parse is the thing that handles it.
#[tokio::test]
async fn p2_passes_when_stream_ends_without_trailing_blank() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.sse_without_trailing_blank = true;
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Pass);
}

/// A user-only transcript must fail the "User and Assistant nodes" check — not sneak through to
/// the model-stamp check (the `||` in the guard is load-bearing).
#[tokio::test]
async fn p2_fails_when_transcript_has_no_assistant() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.conversation_user_only = true;
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P2).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion
            .contains("transcript has User and Assistant nodes"),
        "{}",
        r.assertion
    );
}

// ── P4: spawn under profile grant ─────────────────────────────────────────────

#[tokio::test]
async fn p4_passes_with_profile_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.goals_post_body = json!({
        "session_id": "sess-grant",
        "grant": {"profile": "conformance", "capabilities": ["chat"]}
    });
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P4).await;
    assert_status(&r, PathStatus::Pass);
}

#[tokio::test]
async fn p4_fails_when_grant_profile_falls_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.goals_post_body = json!({
        "session_id": "sess-grant",
        "grant": {"profile": "default", "capabilities": ["chat"]}
    });
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P4).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("grant.profile equals"),
        "{}",
        r.assertion
    );
}

#[tokio::test]
async fn p4_fails_when_grant_capabilities_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.goals_post_body = json!({
        "session_id": "sess-grant",
        "grant": {"profile": "conformance", "capabilities": []}
    });
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P4).await;
    assert_status(&r, PathStatus::Fail);
    assert!(r.assertion.contains("non-empty"), "{}", r.assertion);
}

#[tokio::test]
async fn p4_fails_when_grant_capabilities_is_empty_object() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.goals_post_body = json!({
        "session_id": "sess-grant",
        "grant": {"profile": "conformance", "capabilities": {}}
    });
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P4).await;
    assert_status(&r, PathStatus::Fail);
    assert!(r.assertion.contains("non-empty"), "{}", r.assertion);
}

#[tokio::test]
async fn p4_fails_when_grant_has_no_capabilities_key() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.goals_post_body = json!({
        "session_id": "sess-grant",
        "grant": {"profile": "conformance"}
    });
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P4).await;
    assert_status(&r, PathStatus::Fail);
    assert!(r.assertion.contains("non-empty"), "{}", r.assertion);
}

#[tokio::test]
async fn p4_fails_when_no_session_id_returned() {
    let dir = tempfile::tempdir().unwrap();
    let state = State::healthy(); // goals_post_body = {} → no session_id in the response
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P4).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("returns session_id"),
        "{}",
        r.assertion
    );
}

// ── P5: delegate child (advisory) ─────────────────────────────────────────────

#[tokio::test]
async fn p5_passes_when_child_session_appears() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P5).await;
    assert_status(&r, PathStatus::Pass);
}

#[tokio::test]
async fn p5_fails_when_no_child_spawned() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.delegate_child = false;
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.path_timeout_secs = 3; // keep the deadline poll short

    let r = run(&client, &cfg, PathId::P5).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("child Background session"),
        "{}",
        r.assertion
    );
}

/// The child appears only on the second sessions poll — the pass must come from polling, so a
/// deadline bug (deadline in the past, or the `now < deadline` inversion) fails this test.
#[tokio::test]
async fn p5_passes_when_child_appears_after_polling() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.delegate_child = false; // no immediate child
    state.delegate_child_delay_polls = 2; // appears on the 2nd sessions poll
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P5).await;
    assert_status(&r, PathStatus::Pass);
}

// ── P6: durable outlive + attach + cancel rollback ───────────────────────────

#[tokio::test]
async fn p6_passes_full_durable_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P6).await;
    assert_status(&r, PathStatus::Pass);
    let ev = r.evidence.unwrap();
    assert_eq!(ev["attach_saw_token"], true);
    // Attach serves exactly one session frame then the token: both counters must be exact.
    assert_eq!(ev["attach_event_blocks"], 2);
    assert_eq!(ev["attach_session_frames"], 1);
    assert_ne!(ev["outlive_session"], ev["cancel_session"]);
}

#[tokio::test]
async fn p6_fails_when_attach_has_no_content() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.attach_without_token = true;
    let (client, base) = spawn(state).await;
    let cfg = base_cfg(&base, dir.path());

    let r = run(&client, &cfg, PathId::P6).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("turn content (token/replay)"),
        "{}",
        r.assertion
    );
}

// ── P7: restart honesty ───────────────────────────────────────────────────────

#[cfg(windows)]
fn restart_cmd_exit0() -> String {
    "exit 0".to_string()
}
#[cfg(not(windows))]
fn restart_cmd_exit0() -> String {
    "true".to_string()
}
#[cfg(windows)]
fn restart_cmd_exit1() -> String {
    "exit 1".to_string()
}
#[cfg(not(windows))]
fn restart_cmd_exit1() -> String {
    "false".to_string()
}

#[tokio::test]
async fn p7_skips_without_restart_command() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await;
    let cfg = base_cfg(&base, dir.path()); // restart_command None

    let r = run(&client, &cfg, PathId::P7).await;
    assert_status(&r, PathStatus::Skipped);
    assert!(r.reason.unwrap().contains("restart_command unset"));
}

#[tokio::test]
async fn p7_passes_when_drain_observed_and_lifecycle_honest() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.shutting_down = true; // /api/chat returns 503 from the start
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.restart_command = Some(restart_cmd_exit0());

    let r = run(&client, &cfg, PathId::P7).await;
    assert_status(&r, PathStatus::Pass);
    assert_eq!(r.evidence.unwrap()["saw_shutting_down"], true);
}

#[tokio::test]
async fn p7_fails_when_no_drain_window_observed() {
    let dir = tempfile::tempdir().unwrap();
    let (client, base) = spawn(State::healthy()).await; // shutting_down false → /api/chat 200
    let mut cfg = base_cfg(&base, dir.path());
    cfg.restart_command = Some(restart_cmd_exit0());

    let r = run(&client, &cfg, PathId::P7).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("503 with error=shutting_down"),
        "{}",
        r.assertion
    );
}

#[tokio::test]
async fn p7_fails_when_restart_command_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state.shutting_down = true;
    let (client, base) = spawn(state).await;
    let mut cfg = base_cfg(&base, dir.path());
    cfg.restart_command = Some(restart_cmd_exit1());

    let r = run(&client, &cfg, PathId::P7).await;
    assert_status(&r, PathStatus::Fail);
    assert!(
        r.assertion.contains("execute configured restart_command"),
        "{}",
        r.assertion
    );
}

// ── Binary end-to-end (the actual product) ────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn binary_passing_run_exits_zero_and_writes_report() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = State::healthy();
    state
        .reactions
        .push(reaction("cron:daily-planning", &now_rfc3339()));
    let (_, base) = spawn(state).await;

    let topo = write_topology(dir.path(), TOPOLOGY);
    let cfg_path = dir.path().join("conformance.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "base_url = \"{base}\"\nvault_path = '{}'\ntopology_path = '{}'\n",
            dir.path().join("vault").display(),
            topo.display()
        ),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_liberado-conformance"))
        .args(["--config", cfg_path.to_str().unwrap(), "--path", "p1a"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "exit {:?} stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"status\":\"pass\""), "stdout: {stdout}");
    // Vault report written under conformance/reports/.
    let reports = std::fs::read_dir(dir.path().join("vault").join("conformance").join("reports"))
        .unwrap()
        .count();
    assert_eq!(reports, 1, "one report file for the run");
}

#[tokio::test(flavor = "multi_thread")]
async fn binary_failing_run_exits_one_with_fail_status() {
    let dir = tempfile::tempdir().unwrap();
    // goals_post_body = {} → P4 fails "returns session_id".
    let (_, base) = spawn(State::healthy()).await;

    let cfg_path = dir.path().join("conformance.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "base_url = \"{base}\"\nvault_path = '{}'\n",
            dir.path().join("vault").display()
        ),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_liberado-conformance"))
        .args(["--config", cfg_path.to_str().unwrap(), "--path", "p4"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"status\":\"fail\""), "stdout: {stdout}");
}
