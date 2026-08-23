//! JSONL sink for [`LatencyEvent`]s — the daemon's implementation of
//! [`liberado_provider::LatencyRecorder`].
//!
//! Events are appended to `{LIBERADO_DATA_DIR:-.liberado}/latency/events.jsonl`, one JSON object per
//! line, off the hot path: [`record`](JsonlLatencyRecorder::record) only sends over an unbounded
//! channel; a background task owns the file and does the writes. Best-effort — a full disk or a
//! closed channel drops events rather than ever blocking an inference call.
//!
//! Analyze with `deploy/homelab/latency-report.sh` (p50/p95 per role).

use std::path::PathBuf;
use std::sync::Arc;

use liberado_provider::{LatencyEvent, LatencyRecorder};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// Append-only JSONL recorder with a background writer task.
pub struct JsonlLatencyRecorder {
    tx: mpsc::UnboundedSender<LatencyEvent>,
}

impl JsonlLatencyRecorder {
    /// Spawn the writer task and return a handle. Call once at daemon start.
    pub fn spawn() -> Arc<Self> {
        let dir = PathBuf::from(
            std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into()),
        )
        .join("latency");
        let path = dir.join("events.jsonl");
        let (tx, mut rx) = mpsc::unbounded_channel::<LatencyEvent>();

        tokio::spawn(async move {
            let _ = tokio::fs::create_dir_all(&dir).await;
            let mut file = match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
            {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(),
                        "latency journal disabled (open failed); draining events");
                    while rx.recv().await.is_some() {}
                    return;
                }
            };
            tracing::info!(path = %path.display(), "latency journal active");
            while let Some(event) = rx.recv().await {
                match serde_json::to_string(&event) {
                    Ok(mut line) => {
                        line.push('\n');
                        if let Err(e) = file.write_all(line.as_bytes()).await {
                            tracing::warn!(error = %e, "latency journal write failed");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "latency event serialize failed"),
                }
            }
        });

        Arc::new(Self { tx })
    }
}

impl LatencyRecorder for JsonlLatencyRecorder {
    fn record(&self, event: LatencyEvent) {
        // Best-effort: never block inference; drop if the writer is gone.
        let _ = self.tx.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(correlation: &str) -> LatencyEvent {
        LatencyEvent {
            ts_ms: 1,
            correlation: correlation.into(),
            role: "face",
            model: "test-model".into(),
            kind: "llm_call",
            wall_ms: 5,
            ttft_ms: None,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_prompt_tokens: None,
            finish: "stop".into(),
            tool_calls: 0,
            streamed: false,
            repeat_calls: None,
        }
    }

    /// record() must actually hand the event to the writer: the journal file gains one JSON
    /// line per recorded call. A no-op recorder would silently empty the daemon's only
    /// latency evidence.
    #[tokio::test]
    async fn recorded_events_land_in_the_jsonl_journal() {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY(test): the recorder reads this once at spawn; restored before the test ends.
        unsafe {
            std::env::set_var("LIBERADO_DATA_DIR", dir.path());
        }
        let recorder = JsonlLatencyRecorder::spawn();
        recorder.record(event("corr-marker-1"));
        recorder.record(event("corr-marker-2"));

        let path = dir.path().join("latency").join("events.jsonl");
        let mut contents = String::new();
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            contents = std::fs::read_to_string(&path).unwrap_or_default();
            if contents.matches("corr-marker").count() == 2 {
                break;
            }
        }
        unsafe {
            std::env::remove_var("LIBERADO_DATA_DIR");
        }
        assert_eq!(
            contents.matches("corr-marker").count(),
            2,
            "both recorded events must reach {path:?}: {contents}"
        );
    }
}
