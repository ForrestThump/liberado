//! Gitea client behavior against a hand-rolled single-connection HTTP responder.
//!
//! The mock exists so the wire *shapes* stay pinned — paths (`issues/{n}/comments`, not
//! `pulls/`), the `token` auth scheme, the `Do` verb, and the required-context verdict
//! rule are exactly the details a real forge integration gets silently wrong. One
//! connection serves one canned response; `Connection: close` keeps reqwest from pooling
//! across responses.

use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Recorded {
    request_line: String,
    authorization: Option<String>,
    body: String,
}

struct Mock {
    base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

fn spawn_mock(responses: Vec<(u16, String)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    // Deliberately not joined: leftover accepts die with the test process. Joining would
    // hang whenever reqwest opens more connections than there are canned responses.
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
        base_url: format!("http://{addr}"),
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
        204 => "No Content",
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
    fn forge(&self) -> GiteaForge {
        GiteaForge::new(&self.base_url, "test-token").expect("forge")
    }

    fn taken_requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("record lock").clone()
    }
}

fn sample_pr() -> PrRef {
    PrRef {
        repo: RepoPath("shiloh/bench".into()),
        number: 7,
        url: "http://git.example/shiloh/bench/pulls/7".into(),
    }
}

#[tokio::test]
async fn open_pr_posts_the_full_shape_and_parses_the_ref() {
    let mock = spawn_mock(vec![(
        201,
        r#"{"number": 7, "html_url": "http://git.example/shiloh/bench/pulls/7", "head": {"sha": "abc"}}"#
            .into(),
    )]);
    let pr = mock
        .forge()
        .open_pr(&OpenPr {
            repo: RepoPath("shiloh/bench".into()),
            title: "Add thing".into(),
            head: "delegate/01abc/fix-thing".into(),
            base: "main".into(),
            body: "does the thing".into(),
        })
        .await
        .expect("open pr");

    assert_eq!(pr.number, 7);
    assert_eq!(pr.repo, RepoPath("shiloh/bench".into()));
    assert!(pr.url.ends_with("/pulls/7"));

    let sent = mock.taken_requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].request_line,
        "POST /api/v1/repos/shiloh/bench/pulls HTTP/1.1"
    );
    assert_eq!(sent[0].authorization.as_deref(), Some("token test-token"));
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).expect("json body");
    assert_eq!(body["title"], "Add thing");
    assert_eq!(body["head"], "delegate/01abc/fix-thing");
    assert_eq!(body["base"], "main");
}

#[tokio::test]
async fn comment_goes_through_the_issue_namespace() {
    let mock = spawn_mock(vec![(201, "{}".into())]);
    mock.forge()
        .comment(&sample_pr(), "kickback: fix the flake")
        .await
        .expect("comment");
    let sent = mock.taken_requests();
    assert_eq!(
        sent[0].request_line,
        "POST /api/v1/repos/shiloh/bench/issues/7/comments HTTP/1.1"
    );
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).expect("json body");
    assert_eq!(body["body"], "kickback: fix the flake");
}

/// Two GETs per checks call: the PR (for its head SHA), then that commit's combined status.
#[tokio::test]
async fn checks_resolve_required_names_against_commit_statuses() {
    let mock = spawn_mock(vec![
        (
            200,
            r#"{"number": 7, "html_url": "", "head": {"sha": "deadbee"}}"#.into(),
        ),
        (
            200,
            r#"{"statuses": [
                {"status": "success", "context": "ci/build"},
                {"status": "failure", "context": "ci/lint"}
            ]}"#
            .into(),
        ),
    ]);
    let states = mock
        .forge()
        .checks(
            &sample_pr(),
            &[
                "ci/build".to_string(),
                "ci/lint".to_string(),
                "ci/absent".to_string(),
            ],
        )
        .await
        .expect("checks");

    assert_eq!(
        states.named,
        vec![
            ("ci/build".to_string(), CheckState::Success),
            ("ci/lint".to_string(), CheckState::Failure),
            ("ci/absent".to_string(), CheckState::Pending),
        ]
    );
    assert_eq!(states.overall, CheckState::Failure);

    let sent = mock.taken_requests();
    assert_eq!(
        sent[1].request_line,
        "GET /api/v1/repos/shiloh/bench/commits/deadbee/status HTTP/1.1"
    );
}

#[tokio::test]
async fn a_required_check_missing_from_the_report_is_never_success() {
    let mock = spawn_mock(vec![
        (
            200,
            r#"{"number": 7, "html_url": "", "head": {"sha": "aa11"}}"#.into(),
        ),
        (
            200,
            r#"{"statuses": [{"status": "success", "context": "ci/build"}]}"#.into(),
        ),
    ]);
    let states = mock
        .forge()
        .checks(&sample_pr(), &["ci/build".into(), "ci/unreported".into()])
        .await
        .expect("checks");
    assert_eq!(states.overall, CheckState::Pending);
}

#[tokio::test]
async fn every_required_check_green_is_success() {
    let mock = spawn_mock(vec![
        (
            200,
            r#"{"number": 7, "html_url": "", "head": {"sha": "bb22"}}"#.into(),
        ),
        (
            200,
            r#"{"statuses": [{"status": "success", "context": "one"}, {"status": "warning", "context": "ignored-other"}]}"#.into(),
        ),
    ]);
    let states = mock
        .forge()
        .checks(&sample_pr(), &["one".into()])
        .await
        .expect("checks");
    assert_eq!(states.overall, CheckState::Success);
}

#[tokio::test]
async fn no_required_checks_is_a_vacuous_success() {
    assert_eq!(overall(&[]), CheckState::Success);
}

#[tokio::test]
async fn merge_sends_the_do_verb_and_reads_back_the_commit() {
    let mock = spawn_mock(vec![
        (200, "{}".into()),
        (
            200,
            r#"{"number": 7, "html_url": "", "head": {"sha": "pre"}, "merged_commit_id": "cafe123"}"#
                .into(),
        ),
    ]);
    let commit = mock
        .forge()
        .merge(&sample_pr(), MergeMethod::Squash)
        .await
        .expect("merge");
    assert_eq!(commit.sha, "cafe123");

    let sent = mock.taken_requests();
    assert_eq!(
        sent[0].request_line,
        "POST /api/v1/repos/shiloh/bench/pulls/7/merge HTTP/1.1"
    );
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).expect("json body");
    assert_eq!(body["Do"], "squash");
}

#[tokio::test]
async fn error_status_surfaces_code_and_body() {
    let mock = spawn_mock(vec![(404, r#"{"message": "Not Found"}"#.into())]);
    let error = mock
        .forge()
        .comment(&sample_pr(), "hi")
        .await
        .expect_err("404 must fail");
    match error {
        ForgeError::Status { code, body } => {
            assert_eq!(code, 404);
            assert!(body.contains("Not Found"), "body travels: {body}");
        }
        other => panic!("expected Status, got {other:?}"),
    }
}

#[tokio::test]
async fn state_mapping_covers_giteas_status_vocabulary() {
    assert_eq!(map_state("success"), CheckState::Success);
    assert_eq!(map_state("failure"), CheckState::Failure);
    assert_eq!(map_state("error"), CheckState::Failure);
    assert_eq!(map_state("pending"), CheckState::Pending);
    assert_eq!(map_state("warning"), CheckState::Pending);
}
