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
