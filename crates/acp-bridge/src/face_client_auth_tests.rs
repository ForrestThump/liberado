//! Bearer header on the face-mode daemon request. No process env, no external network.

use super::face_stream_request;
use reqwest::header::AUTHORIZATION;
use std::io::{Read, Write};
use std::time::Duration;

const FAKE_TOKEN: &str = "fake-acp-token-222";

fn authorization(request: &reqwest::Request) -> Option<String> {
    request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn built(token: Option<&str>, session: Option<&str>) -> reqwest::Request {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("test HTTP client");
    let session = session.map(str::to_string);
    face_stream_request(
        &client,
        "http://127.0.0.1:9/api/chat/stream",
        &session,
        "hi",
        token,
    )
    .build()
    .expect("face request")
}

#[tokio::test]
async fn face_request_carries_the_bearer_when_a_token_is_provided() {
    let request = built(Some(FAKE_TOKEN), Some("sess-1"));
    assert_eq!(
        authorization(&request).as_deref(),
        Some("Bearer fake-acp-token-222")
    );
    let body = request
        .body()
        .and_then(|body| body.as_bytes())
        .expect("json body");
    let value: serde_json::Value = serde_json::from_slice(body).expect("json");
    assert_eq!(value["message"], "hi");
    assert_eq!(value["session"], "sess-1");
    assert!(!String::from_utf8_lossy(body).contains(FAKE_TOKEN));
}

#[tokio::test]
async fn face_request_omits_authorization_when_the_token_is_absent() {
    for token in [None, Some(""), Some("   ")] {
        let request = built(token, None);
        assert!(
            authorization(&request).is_none(),
            "blank token must not set Authorization"
        );
        let body = request
            .body()
            .and_then(|body| body.as_bytes())
            .expect("json body");
        let value: serde_json::Value = serde_json::from_slice(body).expect("json");
        assert!(value["session"].is_null());
    }
}

#[tokio::test]
async fn a_live_face_post_sends_the_bearer_and_a_blank_token_does_not() {
    let with_token = capture(Some(FAKE_TOKEN)).await;
    assert!(
        authorization_line(&with_token).is_some_and(|line| line == "Bearer fake-acp-token-222"),
        "face post must send the bearer"
    );
    let without_token = capture(None).await;
    assert!(
        authorization_line(&without_token).is_none(),
        "absent token must not send Authorization"
    );
}

#[test]
fn run_face_turn_uses_the_shared_token_helper_once() {
    let src = include_str!("face_client.rs");
    assert_eq!(src.matches("bearer_token_from_env").count(), 1);
    assert_eq!(src.matches("face_stream_request(").count(), 2);
}

async fn capture(token: Option<&str>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        sock.set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
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
        let _ = sock.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        String::from_utf8_lossy(&buf).into_owned()
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .expect("test HTTP client");
    let endpoint = format!("http://127.0.0.1:{port}/api/chat/stream");
    let response = face_stream_request(&client, &endpoint, &None, "hi", token)
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success());
    server.join().expect("server")
}

fn authorization_line(raw: &str) -> Option<&str> {
    raw.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then_some(value.trim())
    })
}
