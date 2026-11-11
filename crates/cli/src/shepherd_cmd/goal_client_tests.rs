//! Goal HTTP against a local daemon double. No GitHub, no ambient token.

use super::super::Config;
use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

const FAKE_TOKEN: &str = "fake-shepherd-token-111";

#[test]
fn unauthorized_goal_list_is_not_a_free_slot() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, count) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| goal_count(&config(temp.path(), base), Some(FAKE_TOKEN.to_string())),
    );
    let slot = open_slot(count, 2);
    assert!(slot.is_err(), "auth failure must not become a free slot");
    let message = slot.unwrap_err().to_string();
    assert!(
        message.contains("LIBERADO_API_TOKEN"),
        "401 must name the API token variable"
    );
    assert!(message.contains("HTTP 401 Unauthorized"));
    assert!(message.contains("(present but rejected)"));
    assert!(
        !message.contains(FAKE_TOKEN),
        "error included the token: {}",
        message.replace(FAKE_TOKEN, "[redacted]")
    );
    assert_bearer(&raw, FAKE_TOKEN);
    let events = std::fs::read_to_string(temp.path().join(".liberado/shepherd/events.jsonl"))
        .unwrap_or_default();
    assert!(events.contains("daemon_goal_error"));
    assert!(events.contains("LIBERADO_API_TOKEN"));
    assert!(
        !events.contains(FAKE_TOKEN),
        "event log included the token: {}",
        events.replace(FAKE_TOKEN, "[redacted]")
    );
}

#[test]
fn missing_token_on_401_says_the_variable_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, count) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| goal_count(&config(temp.path(), base), None),
    );
    let message = count.unwrap_err().to_string();
    assert!(message.contains("LIBERADO_API_TOKEN"));
    assert!(message.contains("(missing)"));
    assert!(!message.contains("present but rejected"));
    assert!(authorization_line(&raw).is_none());
}

#[test]
fn blank_token_on_401_is_reported_as_missing() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, count) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| goal_count(&config(temp.path(), base), Some("   ".to_string())),
    );
    let message = count.unwrap_err().to_string();
    assert!(message.contains("(missing)"));
    assert!(authorization_line(&raw).is_none());
}

#[test]
fn forbidden_goal_list_names_the_token_variable() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, count) = with_server(403, "Forbidden", "{\"error\":\"forbidden\"}", |base| {
        goal_count(&config(temp.path(), base), Some(FAKE_TOKEN.to_string()))
    });
    let message = count.unwrap_err().to_string();
    assert!(message.contains("HTTP 403 Forbidden"));
    assert!(message.contains("LIBERADO_API_TOKEN"));
    assert!(message.contains("(present but rejected)"));
    assert!(!message.contains(FAKE_TOKEN));
}

#[test]
fn goal_list_counts_open_goals() {
    let temp = tempfile::tempdir().unwrap();
    let body = "[{\"status\":\"running\"},{\"status\":\"succeeded\"},{\"status\":\"PENDING\"},{}]";
    let (_raw, count) = with_server(200, "OK", body, |base| {
        goal_count(&config(temp.path(), base), Some(FAKE_TOKEN.to_string()))
    });
    let count = count.unwrap();
    assert_eq!(count, 2);
    assert!(!open_slot(Ok(count), 2).unwrap());
    assert!(open_slot(Ok(count), 3).unwrap());

    let temp = tempfile::tempdir().unwrap();
    let (_raw, count) = with_server(200, "OK", "[]", |base| {
        goal_count(&config(temp.path(), base), None)
    });
    assert_eq!(count.unwrap(), 0);
    assert!(open_slot(Ok(0), 2).unwrap());
}

#[test]
fn server_error_is_not_zero_goals() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, count) = with_server(
        500,
        "Internal Server Error",
        "{\"error\":\"boom\"}",
        |base| goal_count(&config(temp.path(), base), None),
    );
    let slot = open_slot(count, 2);
    assert!(slot.is_err(), "server error must not become a free slot");
    let message = slot.unwrap_err().to_string();
    assert!(message.contains("HTTP 500"));
    assert!(!message.contains("LIBERADO_API_TOKEN"));
}

#[test]
fn non_list_success_is_not_zero_goals() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, count) = with_server(200, "OK", "{\"error\":\"nope\"}", |base| {
        goal_count(&config(temp.path(), base), None)
    });
    let slot = open_slot(count, 2);
    assert!(slot.is_err(), "a non-list must not become a free slot");
    let message = slot.unwrap_err().to_string();
    assert!(message.contains("not a goal list"));
    assert!(message.contains("HTTP 200"));
}

#[test]
fn invalid_json_is_reported() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, count) = with_server(200, "OK", "not-json", |base| {
        goal_count(&config(temp.path(), base), None)
    });
    let message = count.unwrap_err().to_string();
    assert!(message.contains("not JSON"));
    assert!(message.contains("HTTP 200"));
}

#[test]
fn transport_failure_is_reported() {
    let (listener, port) = must_bind();
    drop(listener);
    let temp = tempfile::tempdir().unwrap();
    let count = goal_count(
        &config(temp.path(), &format!("http://127.0.0.1:{port}")),
        None,
    );
    let slot = open_slot(count, 2);
    assert!(
        slot.is_err(),
        "transport failure must not become a free slot"
    );
    let message = slot.unwrap_err().to_string();
    assert!(message.contains("/api/goals"));
    assert!(message.contains("failed"));
}

#[test]
fn goal_status_keeps_404_as_missing() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, status) = with_server(404, "Not Found", "not-json", |base| {
        status_of(&config(temp.path(), base), "goal-1", None)
    });
    assert_eq!(status.unwrap().as_deref(), Some("missing"));
}

#[test]
fn goal_status_reports_401() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, status) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| {
            status_of(
                &config(temp.path(), base),
                "goal-1",
                Some(FAKE_TOKEN.to_string()),
            )
        },
    );
    let message = status.unwrap_err().to_string();
    assert!(message.contains("/api/goals/goal-1"));
    assert!(message.contains("HTTP 401"));
    assert!(message.contains("LIBERADO_API_TOKEN"));
    assert!(!message.contains(FAKE_TOKEN));
}

#[test]
fn goal_status_reads_a_success_body() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, status) = with_server(200, "OK", "{\"status\":\"Running\"}", |base| {
        status_of(&config(temp.path(), base), "goal-1", None)
    });
    assert_eq!(status.unwrap().as_deref(), Some("running"));
}

#[test]
fn start_goal_returns_the_id_on_success() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, started) = with_server(200, "OK", "{\"id\":\"goal-7\"}", |base| {
        post_goal(
            &config(temp.path(), base),
            Some(FAKE_TOKEN.to_string()),
            &json!({"description": "repair"}),
        )
    });
    assert_eq!(started.unwrap().as_deref(), Some("goal-7"));
    assert!(raw.lines().any(|line| line.starts_with("POST /api/goals ")));
    assert_bearer(&raw, FAKE_TOKEN);
}

#[test]
fn start_goal_reports_401() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, started) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| post_goal(&config(temp.path(), base), None, &json!({})),
    );
    let message = started.unwrap_err().to_string();
    assert!(message.contains("HTTP 401"));
    assert!(message.contains("(missing)"));
    assert!(require_goal_id(Err(bare(message))).is_err());
}

#[test]
fn clean_pr_does_not_take_a_slot_when_auth_fails() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, result) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| {
            let cfg = config(temp.path(), base);
            let mut pr = sample_pr();
            super::super::actions::handle_clean(
                &cfg,
                &mut pr,
                true,
                &std::collections::BTreeSet::new(),
                None,
            )
        },
    );
    let message = result.unwrap_err().to_string();
    assert!(message.contains("HTTP 401"));
    assert!(message.contains("LIBERADO_API_TOKEN"));
    assert!(!message.contains("Bearer "));
    assert!(raw.lines().any(|line| line.starts_with("GET /api/goals ")));
    assert!(!raw.lines().any(|line| line.starts_with("POST ")));
}

#[test]
fn clean_pr_uses_a_free_slot_when_the_list_is_empty() {
    let temp = tempfile::tempdir().unwrap();
    let (raw, result) = with_server(200, "OK", "[]", |base| {
        let cfg = config(temp.path(), base);
        let mut pr = sample_pr();
        super::super::actions::handle_clean(
            &cfg,
            &mut pr,
            true,
            &std::collections::BTreeSet::new(),
            None,
        )
    });
    result.unwrap();
    assert!(raw.lines().any(|line| line.starts_with("GET /api/goals ")));
    assert!(
        !raw.lines().any(|line| line.starts_with("POST ")),
        "dry-run must not start a goal"
    );
}

#[test]
fn failure_action_does_not_kick_back_when_auth_fails() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, result) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| {
            let cfg = config(temp.path(), base);
            let mut pr = sample_pr();
            pr.labels.push(super::super::RERUN.to_string());
            let failures = std::collections::BTreeSet::from(["job|test".to_string()]);
            super::super::actions::handle_new_failures(
                &cfg, &mut pr, true, &failures, &failures, &None,
            )
        },
    );
    let message = result.unwrap_err().to_string();
    assert!(message.contains("HTTP 401"));
    assert!(!message.contains(FAKE_TOKEN));
}

#[test]
fn rerun_does_not_ask_the_daemon_for_a_slot() {
    let (listener, port) = must_bind();
    drop(listener);
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), &format!("http://127.0.0.1:{port}"));
    let mut pr = sample_pr();
    let failures = std::collections::BTreeSet::new();
    super::super::actions::handle_new_failures(&cfg, &mut pr, true, &failures, &failures, &None)
        .unwrap();
}

#[test]
fn seed_does_not_start_a_goal_when_the_count_fails() {
    let temp = tempfile::tempdir().unwrap();
    let (_raw, result) = with_server(
        401,
        "Unauthorized",
        "{\"error\":\"unauthenticated\"}",
        |base| launch_seed_task(&config(temp.path(), base), false, "fix the gate"),
    );
    let message = result.unwrap_err().to_string();
    assert!(message.contains("HTTP 401"));
    assert!(message.contains("LIBERADO_API_TOKEN"));
}

#[test]
fn propagate_tick_error_stops_on_a_daemon_failure_and_logs_other_errors() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:9");
    let daemon = bare("liberado daemon rejected shepherd request to /api/goals: HTTP 401 Unauthorized — set LIBERADO_API_TOKEN (missing)".to_string());
    let stopped = propagate_tick_error(&cfg, 7, Box::new(daemon)).unwrap_err();
    assert!(stopped.to_string().contains("HTTP 401"));

    propagate_tick_error(&cfg, 7, "gh pr checks failed".into()).unwrap();
    let events = std::fs::read_to_string(cfg.state().join("events.jsonl")).unwrap();
    assert!(events.contains("tick_error"));
    assert!(events.contains("gh pr checks failed"));
    assert!(!events.contains("daemon_goal_error"));
}

#[test]
fn persist_started_review_keeps_http_errors_distinct_from_a_missing_id() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:9");
    let missing = super::super::actions::persist_started_review(&cfg, 7, 1, Ok(None)).unwrap();
    assert!(missing.is_none());
    assert!(!super::super::pending(&cfg, 7).exists());

    let error = persist_started_review_error(&cfg);
    assert!(error.contains("HTTP 500"));

    super::super::actions::persist_started_review(&cfg, 7, 1, Ok(Some("sess-1".into()))).unwrap();
    let pending = std::fs::read_to_string(super::super::pending(&cfg, 7)).unwrap();
    assert!(pending.contains("sess-1"));
}

fn persist_started_review_error(cfg: &Config) -> String {
    super::super::actions::persist_started_review(cfg, 7, 1, Err(bare("liberado daemon rejected shepherd request to /api/goals: HTTP 500 Internal Server Error".into())))
        .unwrap_err()
        .to_string()
}

fn bare(message: String) -> DaemonGoalError {
    DaemonGoalError { message }
}

fn sample_pr() -> super::super::Pr {
    super::super::Pr {
        number: 7,
        title: "t".into(),
        branch: "feat".into(),
        base_sha: String::new(),
        head_sha: "abc".into(),
        url: "http://127.0.0.1/pull/7".into(),
        labels: Vec::new(),
    }
}

fn config(root: &std::path::Path, daemon: &str) -> Config {
    Config {
        root: root.to_path_buf(),
        repository: None,
        check_names: Vec::new(),
        daemon: daemon.to_string(),
        project: "liberado".into(),
        base: "main".into(),
        profile: "coding-unattended".into(),
        max_kickbacks: 2,
        cold_reviews: 2,
        cold_turns: 60,
        max_concurrent: 2,
        poll: 120,
    }
}

fn with_server<T>(
    status: u16,
    reason: &str,
    body: &str,
    call: impl FnOnce(&str) -> T,
) -> (String, T) {
    let (listener, port) = must_bind();
    let reason = reason.to_string();
    let body = body.to_string();
    let server = std::thread::spawn(move || serve_one(listener, status, &reason, &body));
    let value = call(&format!("http://127.0.0.1:{port}"));
    let raw = match server.join() {
        Ok(raw) => raw,
        Err(_) => panic!("server thread panicked"),
    };
    (raw, value)
}

fn must_bind() -> (TcpListener, u16) {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => panic!("bind 127.0.0.1:0: {error}"),
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(error) => panic!("local addr: {error}"),
    };
    (listener, port)
}

fn serve_one(listener: TcpListener, status: u16, reason: &str, body: &str) -> String {
    let (mut sock, _) = match listener.accept() {
        Ok(pair) => pair,
        Err(error) => return format!("accept failed: {error}"),
    };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    loop {
        match sock.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(header.as_bytes());
    let _ = sock.write_all(body.as_bytes());
    String::from_utf8_lossy(&buf).into_owned()
}

fn assert_bearer(raw: &str, token: &str) {
    assert_eq!(
        authorization_line(raw),
        Some(format!("Bearer {token}")).as_deref()
    );
}

fn authorization_line(raw: &str) -> Option<&str> {
    raw.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then_some(value.trim())
    })
}
