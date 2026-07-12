//! Session event log + optional durable `CoderTrace` artifacts.
//!
//! Note: event vocabulary is currently coding-specialized (`CoderEvent`). Architecture intends a
//! domain-neutral session event envelope later; until then this module stays thin and local.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use liberado_coder_core::{CoderError, CoderEvent, CoderRunRequest, CoderRunResult, CoderTrace};
use serde_json::Value;

pub type EventLog = Arc<Mutex<Vec<CoderEvent>>>;

pub fn push_event(events: &EventLog, event: CoderEvent) {
    events
        .lock()
        .expect("coder event mutex poisoned")
        .push(event);
}

pub fn snapshot_events(events: &EventLog) -> Vec<CoderEvent> {
    events.lock().expect("coder event mutex poisoned").clone()
}

pub fn preview_value(value: &Value, max_chars: usize) -> String {
    preview_str(&value.to_string(), max_chars)
}

pub fn preview_str(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub fn session_id(request: &CoderRunRequest) -> String {
    format!(
        "{}-attempt-{}-{}",
        safe_segment(&request.task.id),
        request.attempt,
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
    )
}

fn safe_segment(value: &str) -> String {
    let segment = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    let segment = segment.trim_matches('-');
    if segment.is_empty() {
        "session".to_string()
    } else {
        segment.to_string()
    }
}

pub async fn write_trace(
    request: &CoderRunRequest,
    session_id: &str,
    events: Vec<CoderEvent>,
    mut result: Option<CoderRunResult>,
) -> Result<Option<String>, CoderError> {
    let Some(trace_dir) = &request.config.trace_dir else {
        return Ok(None);
    };
    let path = trace_file_path(trace_dir, session_id);
    let path_string = path.to_string_lossy().to_string();
    if let Some(result) = &mut result {
        result.trace_path = Some(path_string.clone());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| {
            CoderError::Setup(format!("create trace dir {}: {e}", parent.display()))
        })?;
    }
    let trace = CoderTrace {
        session_id: session_id.to_string(),
        request: request.clone(),
        events,
        result,
    };
    let bytes = serde_json::to_vec_pretty(&trace)
        .map_err(|e| CoderError::Backend(format!("serialize coder trace: {e}")))?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| CoderError::Backend(format!("write coder trace {}: {e}", path.display())))?;
    Ok(Some(path_string))
}

fn trace_file_path(trace_dir: &str, session_id: &str) -> PathBuf {
    Path::new(trace_dir).join(format!("{session_id}.json"))
}
