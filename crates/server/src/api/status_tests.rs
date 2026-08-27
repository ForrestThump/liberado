//! Split from `status.rs` for module-health boundaries.

use super::*;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

/// `token_usage_total` is **context occupancy of the newest chat turn**, not lifetime spend.
///
/// R2: asserts the parsed `DaemonStatus` field, never a substring of the body.
/// R3: drives the real axum handler, so the state→cost-crate wiring is covered, not just the
/// helper. The data dir comes off `AppState`, so nothing here mutates process environment —
/// `set_var` races every other test in the binary and is `unsafe` for exactly that reason.
///
/// The fixture is chosen so the two candidate answers differ: lifetime sum is 280, context
/// occupancy is 50. A fixture with one event would pass either implementation.
#[tokio::test]
async fn status_token_usage_total_is_context_occupancy_not_lifetime_sum() {
    let dir = tempfile::tempdir().unwrap();
    let latency = dir.path().join("latency");
    std::fs::create_dir_all(&latency).unwrap();
    // Two face turns, then a later subagent call: the subagent is newer but is not the chat's
    // context, so it must not be the number reported.
    std::fs::write(
            latency.join("events.jsonl"),
            r#"{"ts_ms":1,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":200,"completion_tokens":30,"total_tokens":230,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":2,"correlation":"c","role":"face","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":40,"completion_tokens":10,"total_tokens":50,"finish":"stop","tool_calls":0,"streamed":false}
{"ts_ms":3,"correlation":"kid","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":90000,"completion_tokens":5,"finish":"stop","tool_calls":0,"streamed":false}
"#,
        )
        .unwrap();

    // The two readings of this journal are different numbers; this pins which one ships.
    assert_eq!(
        liberado_cost::token_usage_total_for_data_dir(dir.path()),
        Some(90_285),
        "precondition: the cumulative reading is a very different number"
    );

    let root = dir.path().join("sessions-root");
    std::fs::create_dir_all(&root).unwrap();
    let sessions = Arc::new(liberado_session_store::SessionStore::open(&root).await);
    let mut st = crate::state::AppState::for_test(sessions, None, root.clone());
    st.data_dir = dir.path().to_path_buf();
    let state = Arc::new(st);

    let app = axum::Router::new()
        .route("/api/status", axum::routing::get(status))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: DaemonStatus = serde_json::from_slice(&bytes).expect("DaemonStatus");
    assert_eq!(
        body.token_usage_total,
        Some(50),
        "must be the newest face turn's prompt+completion (40+10), not the 90285 total \
             and not the subagent's 90005"
    );
}

/// A journal with no chat turn in it leaves the field absent rather than reporting 0 — the
/// status bar renders this as a percentage of the context window, and a fabricated 0 reads as
/// "empty context" rather than "unknown".
#[tokio::test]
async fn status_token_usage_total_absent_when_no_face_turn() {
    let dir = tempfile::tempdir().unwrap();
    let latency = dir.path().join("latency");
    std::fs::create_dir_all(&latency).unwrap();
    std::fs::write(
            latency.join("events.jsonl"),
            r#"{"ts_ms":1,"correlation":"kid","role":"orchestrator","model":"m","kind":"llm_call","wall_ms":1,"prompt_tokens":700,"completion_tokens":5,"finish":"stop","tool_calls":0,"streamed":false}
"#,
        )
        .unwrap();

    let root = dir.path().join("sessions-root");
    std::fs::create_dir_all(&root).unwrap();
    let sessions = Arc::new(liberado_session_store::SessionStore::open(&root).await);
    let mut st = crate::state::AppState::for_test(sessions, None, root.clone());
    st.data_dir = dir.path().to_path_buf();
    let state = Arc::new(st);

    let app = axum::Router::new()
        .route("/api/status", axum::routing::get(status))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: DaemonStatus = serde_json::from_slice(&bytes).expect("DaemonStatus");
    assert_eq!(body.token_usage_total, None);
}
