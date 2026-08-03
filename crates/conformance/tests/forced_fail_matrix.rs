//! Deliberate broken-daemon matrix for P1a–P4.
//!
//! Each path hits a real [`DaemonClient`] against an in-process mock HTTP surface that returns
//! the broken condition under test. Captures JSON result lines under `FORCED_FAIL_OUT` (or a
//! default next to the test) for PR evidence.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::Path;
use axum::routing::{get, post};
use axum::{Json, Router};
use liberado_conformance::client::DaemonClient;
use liberado_conformance::config::ConformanceConfig;
use liberado_conformance::paths::run_path;
use liberado_conformance::result::{PathId, PathResult, PathStatus};
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[derive(Default)]
struct MockState {
    status: Value,
    reactions: Value,
    goals: Value,
    goal_get: Value,
    conversations: Value,
    sessions: Value,
    hook_accepted: bool,
}

async fn listen(state: Arc<Mutex<MockState>>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/api/status",
            get({
                let s = state.clone();
                move || {
                    let s = s.clone();
                    async move {
                        let g = s.lock().await;
                        Json(g.status.clone())
                    }
                }
            }),
        )
        .route(
            "/api/reactions",
            get({
                let s = state.clone();
                move || {
                    let s = s.clone();
                    async move {
                        let g = s.lock().await;
                        Json(g.reactions.clone())
                    }
                }
            }),
        )
        .route(
            "/api/hooks/{name}",
            post({
                let s = state.clone();
                move || {
                    let s = s.clone();
                    async move {
                        let mut g = s.lock().await;
                        g.hook_accepted = true;
                        (
                            axum::http::StatusCode::ACCEPTED,
                            Json(json!({
                                "accepted": true,
                                "correlation_id": "webhook:conformance:forced-fail-id"
                            })),
                        )
                    }
                }
            }),
        )
        .route(
            "/api/goals",
            post({
                let s = state.clone();
                move || {
                    let s = s.clone();
                    async move {
                        let g = s.lock().await;
                        (axum::http::StatusCode::ACCEPTED, Json(g.goals.clone()))
                    }
                }
            }),
        )
        .route(
            "/api/goals/{id}",
            get({
                let s = state.clone();
                move |Path(_id): Path<String>| {
                    let s = s.clone();
                    async move {
                        let g = s.lock().await;
                        Json(g.goal_get.clone())
                    }
                }
            }),
        )
        .route(
            "/api/goals/{id}/stream",
            get(|| async {
                (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "stream refused",
                )
            }),
        )
        .route(
            "/api/chat/stream",
            post(|| async {
                // Minimal SSE: session then no tokens
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    "event: session\ndata: {\"session\":\"01FORCEDCHAT00000000000000\"}\n\n",
                )
            }),
        )
        .route(
            "/api/conversations/{id}",
            get({
                let s = state.clone();
                move |Path(_id): Path<String>| {
                    let s = s.clone();
                    async move {
                        let g = s.lock().await;
                        Json(g.conversations.clone())
                    }
                }
            }),
        )
        .route(
            "/api/sessions",
            get({
                let s = state.clone();
                move || {
                    let s = s.clone();
                    async move {
                        let g = s.lock().await;
                        Json(g.sessions.clone())
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // Brief settle so connect succeeds.
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

fn cfg(base: &str, vault: PathBuf, topology: Option<PathBuf>) -> ConformanceConfig {
    ConformanceConfig {
        base_url: base.into(),
        budget_secs: 60,
        vault_path: vault,
        topology_path: topology,
        hook_name: "conformance".into(),
        hook_secret_ref: "LIBERADO_HOOK_CONFORMANCE_SECRET".into(),
        profile_name: "conformance".into(),
        paths: vec![],
        advisory_counts: false,
        path_timeout_secs: 5,
        restart_command: None,
    }
}

fn cfg_with_restart(base: &str, vault: PathBuf, restart: Option<&str>) -> ConformanceConfig {
    let mut c = cfg(base, vault, None);
    c.restart_command = restart.map(|s| s.to_string());
    // P7 needs room to poll drain + up; path timeout applies as remaining budget min.
    c.path_timeout_secs = 30;
    c
}

fn out_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FORCED_FAIL_OUT") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/forced-fail-conformance")
}

fn write_capture(name: &str, result: &PathResult) {
    let dir = out_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.json"));
    let line = serde_json::to_string(result).unwrap();
    std::fs::write(&path, format!("{line}\n")).unwrap();
    // Also print so --nocapture captures suite-shaped stdout.
    println!("{line}");
    assert_eq!(
        result.status,
        PathStatus::Fail,
        "{name} must fail under deliberate break"
    );
}

#[tokio::test]
async fn p1a_fails_when_enabled_schedule_has_no_recent_reaction() {
    let dir = tempfile::tempdir().unwrap();
    let topo = dir.path().join("topology.toml");
    // Short period so uptime gate does not skip: every minute.
    std::fs::write(
        &topo,
        r#"
[[schedules]]
name = "minutely-probe"
enabled = true
cron_expr = "0 * * * * * *"
goal = "noop"
"#,
    )
    .unwrap();

    let state = Arc::new(Mutex::new(MockState {
        // 2 hours uptime >> 1.5× 60s
        status: json!({
            "running": true,
            "vault_path": "/vault",
            "uptime_seconds": 7200,
            "watcher_active": true,
            "dispatcher_attached": true,
            "orchestrator_attached": true,
            "reactions_seen": 0,
            "model_name": "test/model",
            "chat_tools": 0,
            "chat_tool_names": [],
            "enter_sends": true
        }),
        reactions: json!([]),
        ..Default::default()
    }));
    // SAFETY(test): single-threaded unit test process env.
    unsafe {
        std::env::set_var("LIBERADO_HOOK_CONFORMANCE_SECRET", "test-secret");
    }
    let addr = listen(state).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg(&base, dir.path().to_path_buf(), Some(topo));
    let r = run_path(
        PathId::P1a,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(30),
    )
    .await;
    write_capture("p1a", &r);
    assert!(
        r.assertion.contains("missing") || r.evidence.is_some(),
        "expected missing/stale schedule evidence: {:?}",
        r
    );
}

#[tokio::test]
async fn p1b_fails_when_session_is_failed_terminal() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(MockState {
        status: json!({
            "running": true,
            "vault_path": "/v",
            "uptime_seconds": 100,
            "watcher_active": true,
            "dispatcher_attached": true,
            "orchestrator_attached": true,
            "reactions_seen": 1,
            "model_name": "test/model",
            "chat_tools": 0,
            "chat_tool_names": [],
            "enter_sends": true
        }),
        reactions: json!([{
            "event_type": "WebhookFired",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "source": "webhook:conformance",
            "correlation_id": "webhook:conformance:forced-fail-id",
            "path": null,
            "outcome": {"dispatched": {"session_id": "sess-fail"}}
        }]),
        goal_get: json!({
            "session": {
                "id": "sess-fail",
                "status": "failed",
                "grant": {"profile": "conformance", "capabilities": {"capabilities": []}}
            }
        }),
        ..Default::default()
    }));
    unsafe {
        std::env::set_var("LIBERADO_HOOK_CONFORMANCE_SECRET", "test-secret");
    }
    let addr = listen(state).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg(&base, dir.path().to_path_buf(), None);
    let r = run_path(
        PathId::P1b,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(30),
    )
    .await;
    write_capture("p1b", &r);
    assert!(
        r.assertion.contains("succeeded") || r.assertion.contains("artifact"),
        "{:?}",
        r.assertion
    );
}

#[tokio::test]
async fn p2_fails_when_assistant_model_stamp_missing() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(MockState {
        status: json!({
            "running": true,
            "vault_path": "/v",
            "uptime_seconds": 100,
            "watcher_active": true,
            "dispatcher_attached": true,
            "orchestrator_attached": true,
            "reactions_seen": 0,
            "model_name": "test/model",
            "chat_tools": 0,
            "chat_tool_names": [],
            "enter_sends": true
        }),
        // chat stream is hardcoded with session only (no tokens) — will fail earlier on tokens.
        // Override: use conversations with user/assistant but no model; need tokens first.
        conversations: json!({
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "pong"}
            ]
        }),
        sessions: json!([{
            "id": "01FORCEDCHAT00000000000000",
            "visibility": "background",
            "status": "running"
        }]),
        ..Default::default()
    }));
    // Custom chat stream with a token event for this test — rebind via full server with token.
    // The default mock has no token; re-run path will fail on token. Build a dedicated app.
    let app = {
        let s = state.clone();
        Router::new()
            .route(
                "/api/status",
                get({
                    let s = s.clone();
                    move || {
                        let s = s.clone();
                        async move { Json(s.lock().await.status.clone()) }
                    }
                }),
            )
            .route(
                "/api/chat/stream",
                post(|| async {
                    (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "text/event-stream",
                        )],
                        "event: session\ndata: {\"session\":\"01FORCEDCHAT00000000000000\"}\n\nevent: token\ndata: {\"text\":\"p\"}\n\n",
                    )
                }),
            )
            .route(
                "/api/conversations/{id}",
                get({
                    let s = s.clone();
                    move |Path(_): Path<String>| {
                        let s = s.clone();
                        async move { Json(s.lock().await.conversations.clone()) }
                    }
                }),
            )
            .route(
                "/api/sessions",
                get({
                    let s = s.clone();
                    move || {
                        let s = s.clone();
                        async move { Json(s.lock().await.sessions.clone()) }
                    }
                }),
            )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg(&base, dir.path().to_path_buf(), None);
    let r = run_path(
        PathId::P2,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(30),
    )
    .await;
    write_capture("p2", &r);
    assert!(
        r.evidence
            .as_ref()
            .and_then(|e| e.get("reason"))
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .contains("no model stamp")
            || r.assertion.contains("model"),
        "{:?}",
        r
    );
}

#[tokio::test]
async fn p3_fails_when_goal_stream_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(MockState {
        reactions: json!([{
            "event_type": "WebhookFired",
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "source": "webhook:conformance",
            "correlation_id": "webhook:conformance:forced-fail-id",
            "path": null,
            "outcome": {"dispatched": {"session_id": "sess-p3"}}
        }]),
        goal_get: json!({"session": {"id": "sess-p3", "status": "running"}}),
        status: json!({
            "running": true,
            "vault_path": "/v",
            "uptime_seconds": 10,
            "watcher_active": true,
            "dispatcher_attached": true,
            "orchestrator_attached": true,
            "reactions_seen": 1,
            "model_name": "m",
            "chat_tools": 0,
            "chat_tool_names": [],
            "enter_sends": true
        }),
        ..Default::default()
    }));
    unsafe {
        std::env::set_var("LIBERADO_HOOK_CONFORMANCE_SECRET", "test-secret");
    }
    let addr = listen(state).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg(&base, dir.path().to_path_buf(), None);
    let r = run_path(
        PathId::P3,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(30),
    )
    .await;
    write_capture("p3", &r);
    assert!(
        r.assertion.contains("stream") || r.assertion.contains("joinable"),
        "{:?}",
        r.assertion
    );
}

#[tokio::test]
async fn p4_fails_when_grant_profile_is_not_conformance() {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(MockState {
        goals: json!({"session_id": "sess-p4", "status": "running"}),
        goal_get: json!({
            "session": {
                "id": "sess-p4",
                "status": "running",
                "grant": {
                    "profile": "life",
                    "capabilities": {"capabilities": [{"Read": {"Vault": "Work"}}]}
                }
            }
        }),
        status: json!({
            "running": true,
            "vault_path": "/v",
            "uptime_seconds": 10,
            "watcher_active": true,
            "dispatcher_attached": true,
            "orchestrator_attached": true,
            "reactions_seen": 0,
            "model_name": "m",
            "chat_tools": 0,
            "chat_tool_names": [],
            "enter_sends": true
        }),
        ..Default::default()
    }));
    let addr = listen(state).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg(&base, dir.path().to_path_buf(), None);
    let r = run_path(
        PathId::P4,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(30),
    )
    .await;
    write_capture("p4", &r);
    assert!(
        r.assertion.contains("profile") || r.assertion.contains("grant"),
        "{:?}",
        r.assertion
    );
}

// ── P6 forced-fail landmines ─────────────────────────────────────────────────

const P6_SESSION: &str = "01P6FORCED0000000000000000";
const P6_CANCEL_SESSION: &str = "01P6CANCEL000000000000000";

/// Modes for the P6 mock surface.
#[derive(Clone, Copy)]
enum P6Break {
    /// After drop, conversation never reports turn_running (pre-durable behaviour).
    DiesOnDisconnect,
    /// turn_running true but attach yields no events / empty body.
    EmptyAttach,
    /// Attach 200 with session framing only — no token/replay (real broken attach shape).
    SessionOnlyAttach,
    /// Attach 200 that streams only *chatter* — progress and tool frames whose free text happens
    /// to mention tokens. Shaped like a real research turn's opening seconds, and the closest
    /// thing to a plausible false pass: nothing of the answer is replayed.
    AttachChatterMentionsTokens,
    /// Cancel HTTP succeeds but turn_running stays true.
    CancelNoOp,
    /// Cancel "succeeds", turn stops, but assistant partial is on the transcript.
    CancelLeavesPartial,
}

struct P6Mock {
    break_mode: P6Break,
    /// How many times GET conversation was called for the outlive session (after stream).
    outlive_gets: Mutex<u32>,
    cancel_posts: Mutex<u32>,
}

fn p6_status() -> Value {
    json!({
        "running": true,
        "vault_path": "/v",
        "uptime_seconds": 100,
        "watcher_active": true,
        "dispatcher_attached": true,
        "orchestrator_attached": true,
        "reactions_seen": 0,
        "model_name": "test/model",
        "chat_tools": 0,
        "chat_tool_names": [],
        "enter_sends": true
    })
}

async fn listen_p6(mock: Arc<P6Mock>) -> SocketAddr {
    let m = mock.clone();
    let app = Router::new()
        .route("/api/status", get(|| async { Json(p6_status()) }))
        .route(
            "/api/chat/stream",
            post(|body: Json<Value>| async move {
                let msg = body
                    .get("message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("");
                // Cancel arm uses CANCEL_PROMPT which mentions "household".
                let id = if msg.contains("household") || msg.contains("numbered list") {
                    P6_CANCEL_SESSION
                } else {
                    P6_SESSION
                };
                let sse = format!("event: session\ndata: {{\"session\":\"{id}\"}}\n\n");
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                )
            }),
        )
        .route(
            "/api/conversations/{id}",
            get({
                let m = m.clone();
                move |Path(id): Path<String>| {
                    let m = m.clone();
                    async move {
                        if id == P6_CANCEL_SESSION {
                            let posts = *m.cancel_posts.lock().await;
                            let (running, messages) = match m.break_mode {
                                P6Break::CancelNoOp => (
                                    true,
                                    json!([{"role": "user", "content": "cancel prompt"}]),
                                ),
                                P6Break::CancelLeavesPartial => (
                                    false,
                                    json!([
                                        {"role": "user", "content": "cancel prompt"},
                                        {"role": "assistant", "content": "partial answer that must not survive cancel"}
                                    ]),
                                ),
                                _ => {
                                    // Healthy cancel arm after cancel: user only.
                                    if posts > 0 {
                                        (
                                            false,
                                            json!([{"role": "user", "content": "cancel prompt"}]),
                                        )
                                    } else {
                                        (
                                            true,
                                            json!([{"role": "user", "content": "cancel prompt"}]),
                                        )
                                    }
                                }
                            };
                            return Json(json!({
                                "messages": messages,
                                "turn_running": running,
                                "turn_unanswered": !running && !matches!(m.break_mode, P6Break::CancelLeavesPartial) && posts > 0,
                            }));
                        }

                        // Outlive session
                        let mut gets = m.outlive_gets.lock().await;
                        *gets += 1;
                        let n = *gets;
                        drop(gets);

                        match m.break_mode {
                            P6Break::DiesOnDisconnect => Json(json!({
                                "messages": [{"role": "user", "content": "durable"}],
                                "turn_running": false,
                                "turn_unanswered": true
                            })),
                            P6Break::EmptyAttach | P6Break::SessionOnlyAttach => {
                                // Stay running forever so attach is attempted; never finish with assistant.
                                Json(json!({
                                    "messages": [{"role": "user", "content": "durable"}],
                                    "turn_running": true,
                                    "turn_unanswered": false
                                }))
                            }
                            _ => {
                                // Healthy outlive: first polls running, later finished with assistant.
                                // After attach the path waits for not running — use get count.
                                if n < 4 {
                                    Json(json!({
                                        "messages": [{"role": "user", "content": "durable"}],
                                        "turn_running": true,
                                        "turn_unanswered": false
                                    }))
                                } else {
                                    Json(json!({
                                        "messages": [
                                            {"role": "user", "content": "durable"},
                                            {"role": "assistant", "content": "full reply on disk"}
                                        ],
                                        "turn_running": false,
                                        "turn_unanswered": false
                                    }))
                                }
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/api/conversations/{id}/attach",
            get({
                let m = m.clone();
                move |Path(_id): Path<String>| {
                    let m = m.clone();
                    async move {
                        match m.break_mode {
                            P6Break::EmptyAttach => (
                                axum::http::StatusCode::OK,
                                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                                // No events at all — empty attach.
                                String::new(),
                            ),
                            P6Break::SessionOnlyAttach => (
                                axum::http::StatusCode::OK,
                                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                                // Real attach shape that is still broken: session framing, zero tokens.
                                "event: session\ndata: {\"session\":\"01P6FORCED0000000000000000\"}\n\n".into(),
                            ),
                            P6Break::AttachChatterMentionsTokens => (
                                axum::http::StatusCode::OK,
                                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                                "event: session\ndata: {\"session\":\"01P6FORCED0000000000000000\"}\n\n\
                                 event: progress\ndata: {\"message\":\"gathering context, 4200 tokens so far\"}\n\n\
                                 event: tool_started\ndata: {\"name\":\"search_web\",\"args_preview\":\"q=token bucket\"}\n\n"
                                    .into(),
                            ),
                            P6Break::DiesOnDisconnect => (
                                axum::http::StatusCode::CONFLICT,
                                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                                "nothing running".into(),
                            ),
                            _ => (
                                axum::http::StatusCode::OK,
                                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                                "event: session\ndata: {\"session\":\"01P6FORCED0000000000000000\"}\n\nevent: token\ndata: {\"text\":\"replay\"}\n\n".into(),
                            ),
                        }
                    }
                }
            }),
        )
        .route(
            "/api/conversations/{id}/cancel",
            post({
                let m = m.clone();
                move |Path(_id): Path<String>| {
                    let m = m.clone();
                    async move {
                        *m.cancel_posts.lock().await += 1;
                        axum::http::StatusCode::OK
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

async fn run_p6_against(break_mode: P6Break) -> PathResult {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(P6Mock {
        break_mode,
        outlive_gets: Mutex::new(0),
        cancel_posts: Mutex::new(0),
    });
    let addr = listen_p6(mock).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg(&base, dir.path().to_path_buf(), None);
    run_path(
        PathId::P6,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(30),
    )
    .await
}

#[tokio::test]
async fn p6_fails_when_turn_dies_on_disconnect() {
    let r = run_p6_against(P6Break::DiesOnDisconnect).await;
    write_capture("p6_dies_on_disconnect", &r);
    assert!(
        r.assertion.contains("turn_running") || r.assertion.contains("durable"),
        "{:?}",
        r.assertion
    );
}

#[tokio::test]
async fn p6_fails_when_attach_replays_nothing() {
    let r = run_p6_against(P6Break::EmptyAttach).await;
    write_capture("p6_empty_attach", &r);
    assert!(
        r.assertion.contains("attach")
            || r.assertion.contains("replay")
            || r.assertion.contains("token"),
        "{:?}",
        r.assertion
    );
}

/// An attach that streams only chatter must fail, even when the chatter says "token".
///
/// The block parser used to set `saw_token` for any frame whose *data* contained the substring,
/// so a `progress` line reporting token counts — or a tool preview searching for "token bucket" —
/// satisfied the content assertion. This is the shape that would have let P6 pass against an
/// attach replaying none of the answer, and free-text frames are common in the first seconds of a
/// real research turn.
#[tokio::test]
async fn p6_fails_when_attach_streams_only_chatter_mentioning_tokens() {
    let r = run_p6_against(P6Break::AttachChatterMentionsTokens).await;
    write_capture("p6_attach_chatter_mentions_tokens", &r);
    assert_eq!(
        r.status,
        PathStatus::Fail,
        "chatter is not the replayed answer; got {:?}",
        r.assertion
    );
    assert!(
        r.assertion.contains("turn content") || r.assertion.contains("token"),
        "must reject on missing turn content; got {:?}",
        r.assertion
    );
    assert_eq!(
        r.evidence
            .as_ref()
            .and_then(|e| e.get("saw_token"))
            .and_then(|v| v.as_bool()),
        Some(false),
        "evidence must show no real token frame arrived: {:?}",
        r.evidence
    );
}

/// Session framing without tokens must fail — that is the live attach shape that made a
/// vacuous `event_blocks > 0` check pass.
#[tokio::test]
async fn p6_fails_when_attach_is_session_framing_only() {
    let r = run_p6_against(P6Break::SessionOnlyAttach).await;
    write_capture("p6_session_only_attach", &r);
    assert_eq!(r.status, PathStatus::Fail);
    assert!(
        r.assertion.contains("attach")
            || r.assertion.contains("token")
            || r.assertion.contains("framing")
            || r.assertion.contains("content")
            || r.assertion.contains("replay"),
        "must reject session-only attach; got {:?}",
        r.assertion
    );
    assert!(
        r.evidence
            .as_ref()
            .and_then(|e| e.get("saw_token"))
            .and_then(|v| v.as_bool())
            == Some(false)
            || r.assertion.contains("token")
            || r.assertion.contains("framing"),
        "evidence/assertion should show missing turn content: {:?}",
        r.evidence
    );
}

#[tokio::test]
async fn p6_fails_when_cancel_does_not_stop_turn() {
    let r = run_p6_against(P6Break::CancelNoOp).await;
    write_capture("p6_cancel_noop", &r);
    // Must fail on cancel-stops or earlier only if outlive broke — healthy outlive then cancel stuck.
    assert_eq!(r.status, PathStatus::Fail);
    assert!(
        r.assertion.contains("cancel")
            || r.assertion.contains("turn_running")
            || r.assertion.contains("stops"),
        "{:?}",
        r.assertion
    );
}

/// The cancel assertion is load-bearing: a partial assistant reply must fail, not pass as "stopped".
#[tokio::test]
async fn p6_fails_when_cancel_leaves_partial_assistant_on_transcript() {
    let r = run_p6_against(P6Break::CancelLeavesPartial).await;
    write_capture("p6_cancel_partial", &r);
    assert_eq!(r.status, PathStatus::Fail);
    assert!(
        r.assertion.contains("persist")
            || r.assertion.contains("assistant")
            || r.assertion.contains("nothing")
            || r.assertion.contains("transcript"),
        "rollback assert must fire on partial reply; got {:?}",
        r.assertion
    );
    // Evidence should show has_assistant true when partial was left.
    if let Some(ev) = &r.evidence {
        let has_assistant = ev
            .get("has_assistant")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(
            has_assistant
                || ev
                    .get("assistant_contents")
                    .and_then(|a| a.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false),
            "evidence should record the partial that broke rollback: {ev}"
        );
    }
}

// ── P7 forced-fail landmines ─────────────────────────────────────────────────

const P7_SESSION: &str = "01P7FORCED0000000000000000";

/// Modes for the P7 mock surface.
#[derive(Clone, Copy)]
enum P7Break {
    /// Never returns shutting_down — new turns always accepted (drain gate missing).
    AcceptsDuringDrain,
    /// After "restart", conversation still reports turn_running (zombie flag).
    StillTurnRunning,
    /// Turn lost: user present, no assistant, turn_unanswered false.
    LostWithoutUnanswered,
}

struct P7Mock {
    break_mode: P7Break,
    /// After restart_command runs, the suite probes chat then history. We flip after first chat POST
    /// so "during drain" probes can still return 503 for healthy modes.
    chat_posts: Mutex<u32>,
}

fn p7_status() -> Value {
    p6_status()
}

async fn listen_p7(mock: Arc<P7Mock>) -> SocketAddr {
    let m = mock.clone();
    let app = Router::new()
        .route("/api/status", get(|| async { Json(p7_status()) }))
        .route(
            "/api/chat/stream",
            post(|_body: Json<Value>| async move {
                let sse = format!(
                    "event: session\ndata: {{\"session\":\"{P7_SESSION}\"}}\n\n"
                );
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    sse,
                )
            }),
        )
        .route(
            "/api/chat",
            post({
                let m = m.clone();
                move |_body: Json<Value>| {
                    let m = m.clone();
                    async move {
                        let n = {
                            let mut g = m.chat_posts.lock().await;
                            *g += 1;
                            *g
                        };
                        match m.break_mode {
                            P7Break::AcceptsDuringDrain => {
                                // Always accept — the landmine: no shutting_down ever.
                                (
                                    axum::http::StatusCode::OK,
                                    Json(json!({"reply": "ok", "session": P7_SESSION})),
                                )
                            }
                            // First probe after restart should see drain; later 200 is fine.
                            P7Break::StillTurnRunning | P7Break::LostWithoutUnanswered => {
                                if n <= 2 {
                                    (
                                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                        Json(json!({
                                            "error": "shutting_down",
                                            "message": "daemon is shutting down; new turns are not accepted"
                                        })),
                                    )
                                } else {
                                    (
                                        axum::http::StatusCode::OK,
                                        Json(json!({"reply": "ok", "session": P7_SESSION})),
                                    )
                                }
                            }
                        }
                    }
                }
            }),
        )
        .route(
            "/api/conversations/{id}",
            get({
                let m = m.clone();
                move |Path(id): Path<String>| {
                    let m = m.clone();
                    async move {
                        let _ = id;
                        let posts = *m.chat_posts.lock().await;
                        // Before restart probes (posts == 0), report turn_running so wait_turn_running
                        // can succeed quickly. After probes begin, apply the broken post-restart shape.
                        let (running, unanswered, messages) = if posts == 0 {
                            (
                                true,
                                false,
                                json!([{"role": "user", "content": "restart prompt"}]),
                            )
                        } else {
                            match m.break_mode {
                                // Zombie flag *isolated*: the reply landed and persisted, but
                                // `turn_running` is still true after a restart. Only the
                                // `!turn_running` half of the honesty check can catch this — if
                                // this case also had no assistant, it would fail via the
                                // lost-turn clause and the zombie guard could be deleted
                                // undetected (it could, until this changed).
                                P7Break::StillTurnRunning => (
                                    true,
                                    false,
                                    json!([
                                        {"role": "user", "content": "restart prompt"},
                                        {"role": "assistant", "content": "finished within grace"}
                                    ]),
                                ),
                                P7Break::LostWithoutUnanswered => (
                                    false,
                                    false,
                                    json!([{"role": "user", "content": "restart prompt"}]),
                                ),
                                // Healthy lifecycle so the path fails on shutting_down only.
                                P7Break::AcceptsDuringDrain => (
                                    false,
                                    true,
                                    json!([{"role": "user", "content": "restart prompt"}]),
                                ),
                            }
                        };
                        Json(json!({
                            "id": P7_SESSION,
                            "turn_running": running,
                            "turn_unanswered": unanswered,
                            "messages": messages,
                        }))
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    addr
}

async fn run_p7_against(break_mode: P7Break) -> PathResult {
    let dir = tempfile::tempdir().unwrap();
    let mock = Arc::new(P7Mock {
        break_mode,
        chat_posts: Mutex::new(0),
    });
    let addr = listen_p7(mock).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    // Host-agnostic no-op restart so the live path runs (does not skip).
    #[cfg(windows)]
    let restart = Some("exit 0");
    #[cfg(not(windows))]
    let restart = Some("true");
    let cfg = cfg_with_restart(&base, dir.path().to_path_buf(), restart);
    run_path(
        PathId::P7,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(45),
    )
    .await
}

#[tokio::test]
async fn p7_fails_when_new_turns_accepted_during_drain() {
    let r = run_p7_against(P7Break::AcceptsDuringDrain).await;
    write_capture("p7_accepts_during_drain", &r);
    assert_eq!(r.status, PathStatus::Fail);
    assert!(
        r.assertion.contains("shutting_down") || r.assertion.contains("503"),
        "must fail on missing drain refusal; got {:?}",
        r.assertion
    );
}

#[tokio::test]
async fn p7_fails_when_turn_running_after_restart() {
    let r = run_p7_against(P7Break::StillTurnRunning).await;
    write_capture("p7_still_turn_running", &r);
    assert_eq!(r.status, PathStatus::Fail);
    assert!(
        r.assertion.contains("turn_running")
            || r.assertion.contains("lifecycle")
            || r.assertion.contains("zombie"),
        "must fail on zombie turn_running; got {:?}",
        r.assertion
    );
}

#[tokio::test]
async fn p7_fails_when_turn_lost_without_unanswered() {
    let r = run_p7_against(P7Break::LostWithoutUnanswered).await;
    write_capture("p7_lost_without_unanswered", &r);
    assert_eq!(r.status, PathStatus::Fail);
    assert!(
        r.assertion.contains("unanswered")
            || r.assertion.contains("assistant")
            || r.assertion.contains("lost")
            || r.assertion.contains("lifecycle"),
        "must fail on silent loss; got {:?}",
        r.assertion
    );
}

/// Unconfigured restart → Skipped with reason; skip is not Pass (suite overall rules).
#[tokio::test]
async fn p7_skips_when_restart_command_unset() {
    let dir = tempfile::tempdir().unwrap();
    // Minimal status-only server so client construction is enough.
    let mock = Arc::new(P7Mock {
        break_mode: P7Break::AcceptsDuringDrain,
        chat_posts: Mutex::new(0),
    });
    let addr = listen_p7(mock).await;
    let base = format!("http://{addr}");
    let client = DaemonClient::new(&base).unwrap();
    let cfg = cfg_with_restart(&base, dir.path().to_path_buf(), None);
    let r = run_path(
        PathId::P7,
        &client,
        &cfg,
        Instant::now() + Duration::from_secs(10),
    )
    .await;
    assert_eq!(r.status, PathStatus::Skipped, "{r:?}");
    assert!(
        r.reason.as_ref().is_some_and(|s| !s.is_empty()),
        "skip must state a reason"
    );
    assert!(
        r.reason
            .as_ref()
            .is_some_and(|s| s.contains("restart_command") || s.contains("opt-in")),
        "reason should mention restart_command: {:?}",
        r.reason
    );
    assert_ne!(r.status, PathStatus::Pass);
    // Suite-level: only skips → overall Skipped, not Pass.
    use liberado_conformance::result::RunReport;
    assert_eq!(
        RunReport::compute_overall(std::slice::from_ref(&r), false),
        PathStatus::Skipped
    );
}
