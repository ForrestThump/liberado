//! Mock-worker tests for the answer subcommand: pin the wire shape (route, bearer
//! header, JSON body) at the same boundary the other delegate cores do.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;

struct Recorded {
    request_line: String,
    authorization: Option<String>,
    body: String,
}

fn spawn_mock(responses: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
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
    (format!("http://{addr}"), requests)
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
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().expect("content length");
        } else if let Some(value) = line.strip_prefix("authorization:") {
            authorization = Some(value.trim().to_string());
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).expect("body");
    }
    Recorded {
        request_line,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

fn write_response(stream: &std::net::TcpStream, status: u16, body: &str) {
    let mut stream = stream;
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-length:{}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).expect("write");
}

/// The core takes a [`Connection`] directly, so no process env is touched.
#[tokio::test]
async fn answer_posts_the_contract_shape_to_the_answers_route() {
    let (endpoint, requests) = spawn_mock(vec![(200, r#"{"delivered": true}"#.to_string())]);
    let connection = super::Connection {
        endpoint,
        token: "test-token".into(),
    };
    let ack = post_answer(
        &connection,
        &liberado_delegate_contract::Answer {
            question_id: "01Q".into(),
            kind: AnswerKind::Question,
            chosen_option: Some("left".into()),
            body: "go left".into(),
        },
        "01TASK",
    )
    .await
    .expect("answer posts");
    assert!(ack.delivered);

    let taken = requests.lock().unwrap();
    assert_eq!(taken.len(), 1);
    assert!(
        taken[0]
            .request_line
            .contains("POST /v1/delegate/tasks/01TASK/answers"),
        "{}",
        taken[0].request_line
    );
    assert_eq!(
        taken[0].authorization.as_deref(),
        Some("Bearer test-token"),
        "answers are token-protected like every route"
    );
    assert!(
        taken[0].body.contains(r#""question_id":"01Q""#),
        "{}",
        taken[0].body
    );
}
