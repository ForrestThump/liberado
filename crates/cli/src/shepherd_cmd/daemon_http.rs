//! Daemon API requests for `liberado shepherd`.
//!
//! Goal routes are the only callers. GitHub access stays on the `gh` CLI in the
//! parent module. That command uses `gh`'s own credentials and must not receive
//! the Liberado API token.

use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::AUTHORIZATION;

use chat_client_contract::api_auth::authorization_value;

pub(super) fn daemon_get(
    http: &Client,
    url: &str,
    token: impl FnOnce() -> Option<String>,
) -> RequestBuilder {
    with_bearer(http.get(url), token())
}

pub(super) fn daemon_post(
    http: &Client,
    url: &str,
    token: impl FnOnce() -> Option<String>,
) -> RequestBuilder {
    with_bearer(http.post(url), token())
}

fn with_bearer(builder: RequestBuilder, token: Option<String>) -> RequestBuilder {
    match authorization_value(token.as_deref()) {
        Some(value) => builder.header(AUTHORIZATION, value),
        None => builder,
    }
}

#[cfg(test)]
mod tests {
    use super::{daemon_get, daemon_post};
    use reqwest::header::AUTHORIZATION;
    use std::io::{Read, Write};
    use std::time::Duration;

    const FAKE_TOKEN: &str = "fake-shepherd-token-111";

    fn http() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("test HTTP client")
    }

    fn authorization(request: &reqwest::blocking::Request) -> Option<String> {
        request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    #[test]
    fn daemon_requests_carry_the_bearer_when_a_token_is_provided() {
        let http = http();
        let get_request = daemon_get(&http, "http://127.0.0.1:9/api/goals", || {
            Some(FAKE_TOKEN.to_string())
        })
        .build()
        .expect("get request");
        let post_request = daemon_post(&http, "http://127.0.0.1:9/api/goals", || {
            Some(FAKE_TOKEN.to_string())
        })
        .build()
        .expect("post request");

        assert_eq!(
            authorization(&get_request).as_deref(),
            Some("Bearer fake-shepherd-token-111")
        );
        assert_eq!(
            authorization(&post_request).as_deref(),
            Some("Bearer fake-shepherd-token-111")
        );
        assert_eq!(get_request.method(), reqwest::Method::GET);
        assert_eq!(post_request.method(), reqwest::Method::POST);
        assert_eq!(get_request.url().path(), "/api/goals");
    }

    #[test]
    fn daemon_requests_omit_authorization_when_the_token_is_absent() {
        let http = http();
        for token in [None, Some(String::new()), Some("   ".to_string())] {
            let supplied = token.clone();
            let request = daemon_get(&http, "http://127.0.0.1:9/api/goals/goal-1", move || {
                supplied.clone()
            })
            .build()
            .expect("get request");
            assert!(
                authorization(&request).is_none(),
                "blank token must not set Authorization"
            );
            let supplied = token.clone();
            let request = daemon_post(&http, "http://127.0.0.1:9/api/goals", move || supplied)
                .build()
                .expect("post request");
            assert!(
                authorization(&request).is_none(),
                "blank token must not set Authorization"
            );
        }
    }

    #[test]
    fn a_live_daemon_get_sends_the_bearer_and_a_blank_token_does_not() {
        let with_token = capture(|port| {
            let url = format!("http://127.0.0.1:{port}/api/goals/goal-9");
            let _ = daemon_get(&http(), &url, || Some(FAKE_TOKEN.to_string())).send();
        });
        assert!(
            authorization_line(&with_token)
                .is_some_and(|line| line == "Bearer fake-shepherd-token-111"),
            "daemon get must send the bearer"
        );

        let without_token = capture(|port| {
            let url = format!("http://127.0.0.1:{port}/api/goals");
            let _ = daemon_get(&http(), &url, || None).send();
        });
        assert!(
            authorization_line(&without_token).is_none(),
            "absent token must not send Authorization"
        );
    }

    #[test]
    fn github_helpers_do_not_name_the_daemon_token() {
        let parent = include_str!("../shepherd_cmd.rs");
        assert_eq!(
            parent.matches("bearer_token_from_env").count(),
            0,
            "daemon goal calls live in goal_client"
        );
        assert!(!parent.contains("LIBERADO_API_TOKEN"));
        let goals = include_str!("goal_client.rs");
        assert_eq!(
            goals.matches("bearer_token_from_env").count(),
            3,
            "the token lookup stays on the three daemon goal calls"
        );
        assert!(
            !goals.contains("LIBERADO_API_TOKEN"),
            "name the variable through API_TOKEN_ENV"
        );
        assert!(!goals.contains("std_command"));
        assert!(!goals.contains("\"gh\""));
        assert_daemon_token_absent(include_str!("actions.rs"));
        assert_daemon_token_absent(include_str!("review_observer.rs"));
        assert_daemon_token_absent(include_str!("tick_support.rs"));
        assert_daemon_token_absent(include_str!("record.rs"));
    }

    fn assert_daemon_token_absent(src: &str) {
        assert!(!src.contains("bearer_token_from_env"));
        assert!(!src.contains("LIBERADO_API_TOKEN"));
        assert!(!src.contains("daemon_get"));
        assert!(!src.contains("daemon_post"));
    }

    fn capture(send: impl FnOnce(u16)) -> String {
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
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
            String::from_utf8_lossy(&buf).into_owned()
        });
        send(port);
        server.join().expect("server")
    }

    fn authorization_line(raw: &str) -> Option<&str> {
        raw.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then_some(value.trim())
        })
    }
}
