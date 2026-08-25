//! The delegate client's HTTP cores against a hand-rolled mock worker. Same discipline
//! as the forge tests: pin the wire shapes (bearer header, paths, duplicate handling,
//! error surfacing) at the boundary where a real integration goes silently wrong.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::{Connection, fetch_health, fetch_task, submit_from_file};

#[derive(Clone)]
struct Recorded {
    request_line: String,
    authorization: Option<String>,
    body: String,
}

struct Mock {
    endpoint: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

/// One canned response per connection, served in order.
fn spawn_mock(responses: Vec<(u16, String)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    // Not joined on purpose: spare accepts die with the test process (see the forge
    // tests for why joining would hang).
    std::thread::spawn(move || {
        for (status, body) in responses {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            recorded
                .lock()
                .expect("record lock")
                .push(read_request(&stream));
            write_response(&stream, status, &body);
        }
    });
    Mock {
        endpoint: format!("http://{addr}"),
        requests,
    }
}

fn read_request(stream: &std::net::TcpStream) -> Recorded {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut request_line = String::new();
    reader.read_line(&mut request_line).expect("request line");
    let mut authorization = None;
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("header line");
        if line.trim().is_empty() {
            break;
        }
        let lowered = line.to_ascii_lowercase();
        if lowered.starts_with("authorization:") {
            authorization = Some(line.split_once(':').expect("colon").1.trim().to_string());
        }
        if let Some(value) = lowered.strip_prefix("content-length:") {
            content_length = value.trim().parse().expect("content-length");
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).expect("body bytes");
    }
    Recorded {
        request_line: request_line.trim().to_string(),
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn write_response(stream: &std::net::TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let mut stream = stream;
    stream
        .write_all(response.as_bytes())
        .expect("write response");
}

impl Mock {
    fn connection(&self) -> Connection {
        Connection {
            endpoint: self.endpoint.clone(),
            token: "test-token".into(),
        }
    }

    fn taken(&self) -> Vec<Recorded> {
        self.requests.lock().expect("record lock").clone()
    }
}

/// A stored TaskRecord with one variable part: its status object.
const RECORD_PREFIX: &str = r#"{"spec": {"id": "01JTEST", "project": "p", "repository": "o/r",
    "base_branch": "main", "goal": "g",
    "acceptance": {"preflight": [], "required_checks": [], "forbidden_paths": []},
    "budget": {}, "grant": {"max_kickbacks": 2, "forbidden_paths": []}},
    "session_id": null,"#;

fn record_json(status_state: &str) -> String {
    format!(
        "{RECORD_PREFIX} \"status\": {status_state}, \"pr_url\": null, \
         \"updated_at\": \"2026-08-25T00:00:00Z\"}}"
    )
}

fn queued_outcome(duplicate: bool) -> String {
    let record = record_json(r#"{"state": "queued", "detail": null}"#);
    format!(r#"{{"record": {record}, "duplicate": {duplicate}}}"#)
}

fn write_task_file(dir: &std::path::Path, contents: &str) -> String {
    let path = dir.join("task.json");
    std::fs::write(&path, contents).expect("write task file");
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn submit_posts_the_spec_and_reports_a_fresh_task() {
    let mock = spawn_mock(vec![(201, queued_outcome(false))]);
    let dir = tempfile::tempdir().unwrap();
    let file = write_task_file(
        dir.path(),
        r#"{"id":"01JTEST","project":"p","repository":"o/r","base_branch":"main","goal":"g","acceptance":{"preflight":[],"required_checks":[],"forbidden_paths":[]}}"#,
    );

    let text = submit_from_file(&mock.connection(), &file)
        .await
        .expect("submit");
    assert!(text.starts_with("submitted task 01JTEST"), "{text}");
    assert!(
        text.contains("\"state\": \"queued\"") || text.contains("queued"),
        "{text}"
    );

    let sent = mock.taken();
    assert_eq!(sent[0].request_line, "POST /v1/delegate/tasks HTTP/1.1");
    assert_eq!(sent[0].authorization.as_deref(), Some("Bearer test-token"));
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!(body["id"], "01JTEST");
}

#[tokio::test]
async fn a_replayed_submit_reports_the_duplicate_no_op() {
    let mock = spawn_mock(vec![(200, queued_outcome(true))]);
    let dir = tempfile::tempdir().unwrap();
    let file = write_task_file(
        dir.path(),
        r#"{"id":"01JTEST","project":"p","repository":"o/r","base_branch":"main","goal":"g","acceptance":{"preflight":[],"required_checks":[],"forbidden_paths":[]}}"#,
    );

    let text = submit_from_file(&mock.connection(), &file)
        .await
        .expect("resubmit");
    assert!(
        text.starts_with("duplicate submit ignored"),
        "redelivery must read as a no-op: {text}"
    );
}

#[tokio::test]
async fn a_file_that_is_not_a_taskspec_is_rejected_before_any_http() {
    let mock = spawn_mock(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let file = write_task_file(dir.path(), "{ not a spec }");

    let error = submit_from_file(&mock.connection(), &file)
        .await
        .expect_err("malformed spec must fail");
    assert!(error.contains("is not a TaskSpec"), "{error}");
    assert!(mock.taken().is_empty(), "no request may leave the machine");
}

#[tokio::test]
async fn worker_error_status_surfaces_code_and_body() {
    let mock = spawn_mock(vec![(403, r#"{"message":"unknown token"}"#.into())]);
    let dir = tempfile::tempdir().unwrap();
    let file = write_task_file(
        dir.path(),
        r#"{"id":"01JTEST","project":"p","repository":"o/r","base_branch":"main","goal":"g","acceptance":{"preflight":[],"required_checks":[],"forbidden_paths":[]}}"#,
    );

    let error = submit_from_file(&mock.connection(), &file)
        .await
        .expect_err("403 must fail");
    assert!(error.contains("worker returned 403"), "{error}");
    assert!(error.contains("unknown token"), "{error}");
}

#[tokio::test]
async fn status_fetches_the_record_by_path() {
    let mock = spawn_mock(vec![(
        200,
        record_json(r#"{"state": "running", "detail": null}"#),
    )]);
    let record = fetch_task(&mock.connection(), "01JTEST")
        .await
        .expect("status");
    assert_eq!(record.spec.id.0, "01JTEST");
    assert_eq!(
        record.status,
        liberado_delegate_contract::TaskStatus::Running
    );
    assert_eq!(
        mock.taken()[0].request_line,
        "GET /v1/delegate/tasks/01JTEST HTTP/1.1"
    );
}

#[tokio::test]
async fn health_parses_the_fingerprint_payload() {
    let mock = spawn_mock(vec![(
        200,
        r#"{"status":"ok","version":"0.1.0","fingerprint":"0.1.0+abc123"}"#.into(),
    )]);
    let health = fetch_health(&mock.connection()).await.expect("health");
    assert_eq!(health.fingerprint, "0.1.0+abc123");
    assert_eq!(
        mock.taken()[0].request_line,
        "GET /v1/delegate/health HTTP/1.1"
    );
}

// --- argument grammar -----------------------------------------------------

#[test]
fn parse_flags_takes_positional_and_flags() {
    let (positional, flags) = super::parse_flags(
        [
            "task.json".to_string(),
            "--endpoint".to_string(),
            "http://w:7780".to_string(),
        ]
        .into_iter(),
        "task.json path",
    )
    .expect("parses");
    assert_eq!(positional.as_deref(), Some("task.json"));
    assert_eq!(
        flags,
        super::Flags {
            endpoint: Some("http://w:7780".into()),
            token_env: None,
        }
    );
}

#[test]
fn parse_flags_rejects_unknown_and_dangling_flags() {
    let error = super::parse_flags(["--bogus".to_string()].into_iter(), "task-id")
        .expect_err("unknown flag must be an error, never a fall-through");
    assert!(error.contains("bogus"));

    let error = super::parse_flags(["--endpoint".to_string()].into_iter(), "task-id")
        .expect_err("dangling value must be an error");
    assert!(error.contains("endpoint"));
}

#[test]
fn a_second_positional_is_rejected() {
    let error = super::parse_flags(["a".to_string(), "b".to_string()].into_iter(), "task-id")
        .expect_err("two positionals is a usage error");
    assert!(error.contains("exactly one"));
}
