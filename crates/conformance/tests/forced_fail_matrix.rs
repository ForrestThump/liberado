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
                        (
                            axum::http::StatusCode::ACCEPTED,
                            Json(g.goals.clone()),
                        )
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
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/event-stream",
                    )],
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
    }
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
    let r = run_path(PathId::P1a, &client, &cfg, Instant::now() + Duration::from_secs(30)).await;
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
    let r = run_path(PathId::P1b, &client, &cfg, Instant::now() + Duration::from_secs(30)).await;
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
    let r = run_path(PathId::P2, &client, &cfg, Instant::now() + Duration::from_secs(30)).await;
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
    let r = run_path(PathId::P3, &client, &cfg, Instant::now() + Duration::from_secs(30)).await;
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
    let r = run_path(PathId::P4, &client, &cfg, Instant::now() + Duration::from_secs(30)).await;
    write_capture("p4", &r);
    assert!(
        r.assertion.contains("profile") || r.assertion.contains("grant"),
        "{:?}",
        r.assertion
    );
}
