//! `liberado delegate watch` — the SSE consumer half of the delegate client.
//!
//! Split from `delegate_cmd.rs` to stay under its module-health boundary. Termination
//! comes from the contract's own [`WorkerEvent::is_terminal`], never a second copy of
//! the rule; frames decode through the shared SSE parser.

use std::error::Error;

use super::{connection, emit, parse_flags, request, usage};
use liberado_delegate_contract::{WorkerEvent, routes};

/// `liberado delegate watch <task-id>` — attach to the worker's event stream and
/// print frames until the terminal one. Transport here; decoding/termination rules
/// live in [`consume`] where they are testable.
pub(super) async fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    use futures::StreamExt;

    let (id, flags) = parse_flags(&mut args, "task-id").map_err(|error| usage(&error))?;
    let id = id.ok_or_else(|| usage("watch needs a task-id"))?;
    let connection = connection(&flags)?;
    let path = routes::task_events(&id);
    let response = request(&connection, reqwest::Method::GET, &path)
        .send()
        .await
        .map_err(|error| format!("stream {path}: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "worker returned {status}: {}",
            response.text().await.unwrap_or_default()
        )
        .into());
    }

    let text_chunks = response.bytes_stream().map(|chunk| {
        chunk
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|error| error.to_string())
    });
    consume(text_chunks).await.map_err(Into::into)
}

/// Decode framed SSE chunks, print each as `[kind] correlation`, and stop at the
/// contract's own notion of terminal. Generic over the chunk source so tests feed
/// plain strings.
async fn consume<S>(chunks: S) -> Result<(), String>
where
    S: futures::Stream<Item = Result<String, String>> + Unpin,
{
    use futures::StreamExt;

    let mut decoder = chat_client_contract::native::SseDecoder::default();
    tokio::pin!(chunks);
    while let Some(chunk) = chunks.next().await {
        for frame in decoder.push(&chunk?) {
            match serde_json::from_str::<WorkerEvent>(&frame.data) {
                Ok(event) => {
                    let terminal = event.is_terminal();
                    emit(&format!("[{}] {}", frame.event, event.correlation_id));
                    if terminal {
                        return Ok(());
                    }
                }
                Err(_) => emit(&format!("[{}] {}", frame.event, frame.data)),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::consume;
    use futures::stream;

    fn sse(event: &str, data: &str) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    fn worker_event_json(kind: &str, correlation: &str, state: &str) -> String {
        format!(
            r#"{{"kind":"{kind}","correlation_id":"{correlation}","task_id":"t","payload":{{"status":{{"state":"{state}"}}}}}}"#
        )
    }

    fn pr_ready_json(correlation: &str) -> String {
        worker_event_json("pr_ready", correlation, "pr_opened")
    }

    fn chunks(
        items: Vec<Result<String, String>>,
    ) -> impl futures::Stream<Item = Result<String, String>> + Unpin {
        stream::iter(items)
    }

    /// Capture `emit` output by swapping in a buffer through the same code path the
    /// CLI uses: `consume` writes via `emit`, which prints to stdout — so these tests
    /// assert on termination behavior and keep rendering pinned in delegate_cmd's own
    /// frame tests.
    #[tokio::test]
    async fn stops_at_the_terminal_event_without_draining_the_rest() {
        let mut frames = vec![
            Ok(sse(
                "status_changed",
                &worker_event_json("status_changed", "delegate:t:1", "queued"),
            )),
            Ok(sse("pr_ready", &pr_ready_json("delegate:t:2"))),
            // A well-behaved server closes after terminal; a chatty one must not matter.
            Ok(sse(
                "status_changed",
                &worker_event_json("status_changed", "delegate:t:3", "running"),
            )),
        ];
        frames.push(Err("post-terminal traffic must be unreachable".into()));
        consume(chunks(frames)).await.expect("terminal stop");
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_terminal_is_not_an_error() {
        let frames = vec![Ok(sse(
            "status_changed",
            &worker_event_json("status_changed", "delegate:t:1", "running"),
        ))];
        consume(chunks(frames)).await.expect("clean end");
    }

    #[tokio::test]
    async fn transport_errors_surface_as_errors() {
        let result = consume(chunks(vec![Err("connection reset".into())])).await;
        assert_eq!(result, Err("connection reset".into()));
    }
}
