//! Append-only JSONL journals for face → dispatcher delegations.
//!
//! Files live under `{LIBERADO_DATA_DIR:-.liberado}/dispatches/<correlation_id>.jsonl` and are
//! linked from the parent chat by `correlation_id` (and optional `parent_conversation` in the
//! header). These are **ops/debug artifacts**, never injected into model context.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{Value, json};

/// Directory for delegation journals (under the liberado data dir).
pub fn dispatches_dir() -> PathBuf {
    PathBuf::from(std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into()))
        .join("dispatches")
}

/// Path for one delegation's journal file.
pub fn journal_path(correlation_id: &str) -> PathBuf {
    // Correlation ids are safe path segments (`chat-delegate-<ulid>`).
    dispatches_dir().join(format!("{correlation_id}.jsonl"))
}

/// Best-effort append of one JSON object line. Failures are logged and ignored.
pub async fn append(correlation_id: &str, record: Value) {
    let path = journal_path(correlation_id);
    if !ensure_parent(&path).await {
        return;
    }
    let mut line = record.to_string();
    line.push('\n');
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(mut f) => {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = f.write_all(line.as_bytes()).await {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "dispatch journal write failed"
                );
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "dispatch journal open failed");
        }
    }
}

/// Create the journal's parent directory. `false` means the write is abandoned (and already
/// logged); the caller must not proceed.
async fn ensure_parent(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return true;
    };
    match tokio::fs::create_dir_all(parent).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %parent.display(),
                "dispatch journal mkdir failed"
            );
            false
        }
    }
}

/// Header record written at the start of a delegation.
pub fn start_record(
    correlation_id: &str,
    parent_conversation: Option<&str>,
    goal: &str,
    model: Option<&str>,
) -> Value {
    json!({
        "ts": Utc::now().to_rfc3339(),
        "kind": "start",
        "correlation_id": correlation_id,
        "parent_conversation": parent_conversation,
        "goal": goal,
        "model": model,
        "journal": journal_path(correlation_id).display().to_string(),
    })
}

// There is no `decision_record` here any more. The dispatch decision is no longer journal-only: the
// dispatch pack records it as an assistant turn on the session (one-execution-engine E2/E4), which
// makes it searchable and forkable like any other turn. The journal keeps the start/disposition
// bookends; the reasoning lives in the transcript.

pub fn disposition_record(summary: &str, model: Option<&str>) -> Value {
    json!({
        "ts": Utc::now().to_rfc3339(),
        "kind": "disposition",
        "model": model,
        "summary": summary,
    })
}

/// Relative path hint for humans (from repo / cwd).
pub fn journal_display_path(correlation_id: &str) -> String {
    Path::new(".liberado")
        .join("dispatches")
        .join(format!("{correlation_id}.jsonl"))
        .display()
        .to_string()
}

#[cfg(test)]
#[path = "dispatch_journal/tests.rs"]
pub(crate) mod survivor_tests;
