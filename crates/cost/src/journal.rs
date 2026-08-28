//! Load latency JSONL and dispatch-journal parent links.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

/// One inference call as recorded in `<data>/latency/events.jsonl`.
///
/// Mirrors the provider's `LatencyEvent` wire shape for deserialization only — this crate does not
/// re-instrument or write journals.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JournalEvent {
    pub ts_ms: u64,
    pub correlation: String,
    pub role: String,
    pub model: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub wall_ms: u64,
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    #[serde(default)]
    pub prompt_tokens: Option<u32>,
    #[serde(default)]
    pub completion_tokens: Option<u32>,
    #[serde(default)]
    pub total_tokens: Option<u32>,
    /// Absent means the backend volunteered nothing — distinct from a reported zero.
    #[serde(default)]
    pub cached_prompt_tokens: Option<u32>,
    #[serde(default)]
    pub finish: String,
    #[serde(default)]
    pub tool_calls: usize,
    #[serde(default)]
    pub streamed: bool,
    #[serde(default)]
    pub repeat_calls: Option<usize>,
}

fn default_kind() -> String {
    "llm_call".into()
}

#[derive(Debug, Error)]
pub enum LoadError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("json parse error in {path} line {line}: {source}")]
    Json {
        path: String,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Load latency events from a JSONL file. Missing file → empty list (not an error) so a fresh
/// install still runs the tool.
pub fn load_latency_events(path: &Path) -> Result<Vec<JournalEvent>, LoadError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path).map_err(|source| LoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    load_latency_events_reader(path.display().to_string(), BufReader::new(file))
}

/// Parse latency events from an in-memory JSONL string (tests / fixtures).
pub fn load_latency_events_from_str(src: &str) -> Result<Vec<JournalEvent>, LoadError> {
    load_latency_events_reader("<memory>".into(), src.as_bytes())
}

fn load_latency_events_reader<R: BufRead>(
    path_label: String,
    reader: R,
) -> Result<Vec<JournalEvent>, LoadError> {
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(|source| LoadError::Io {
            path: path_label.clone(),
            source,
        })?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: JournalEvent = serde_json::from_str(line).map_err(|source| LoadError::Json {
            path: path_label.clone(),
            line: idx + 1,
            source,
        })?;
        out.push(event);
    }
    Ok(out)
}

/// How much of the journal's tail [`load_latency_events_tail`] reads. Sized to hold well over a
/// hundred records at observed line lengths (~300 B), which is far more than the "most recent turn"
/// question needs.
pub const TAIL_SCAN_BYTES: u64 = 256 * 1024;

/// Load only the **last** `TAIL_SCAN_BYTES` worth of complete records.
///
/// The journal is append-only and unbounded, and `/api/status` is polled every few seconds by every
/// connected client. Reading and parsing the whole file per poll is O(history) forever, so callers
/// that only need recent state read the tail instead.
///
/// The first line in the window is dropped unless the window starts exactly at a record boundary —
/// it is almost certainly a partial record. A malformed line anywhere in the window yields `None`
/// rather than an error: this feeds a status field, which must never fail a request over a journal
/// it does not own.
pub fn load_latency_events_tail(path: &Path, max_bytes: u64) -> Option<Vec<JournalEvent>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(max_bytes.min(len) as usize);
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut lines: Vec<&str> = text.lines().collect();
    // Truncated head: only when we did not start at byte 0 is the first line suspect.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let mut out = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str::<JournalEvent>(line).ok()?);
    }
    Some(out)
}

/// First-line dispatch start record fields we care about.
#[derive(Debug, Deserialize)]
struct DispatchStart {
    /// Present on real journals (`"start"`); accepted and ignored for forward-compat.
    #[serde(default)]
    #[allow(dead_code)]
    kind: Option<String>,
    #[serde(default)]
    correlation_id: Option<String>,
    #[serde(default)]
    parent_conversation: Option<String>,
}

/// Scan `{dispatches_dir}/*.jsonl` and build `child_correlation → parent_conversation`.
///
/// Only the first non-empty line of each file is considered (the start record). Missing directory
/// → empty map.
pub fn load_dispatch_parent_map(
    dispatches_dir: &Path,
) -> Result<HashMap<String, String>, LoadError> {
    let mut map = HashMap::new();
    if !dispatches_dir.is_dir() {
        return Ok(map);
    }
    let entries = std::fs::read_dir(dispatches_dir).map_err(|source| LoadError::Io {
        path: dispatches_dir.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| LoadError::Io {
            path: dispatches_dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some((child, parent)) = read_parent_from_dispatch_file(&path)? {
            map.insert(child, parent);
        }
    }
    Ok(map)
}

fn read_parent_from_dispatch_file(path: &Path) -> Result<Option<(String, String)>, LoadError> {
    let file = File::open(path).map_err(|source| LoadError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut first = String::new();
    loop {
        first.clear();
        let n = reader
            .read_line(&mut first)
            .map_err(|source| LoadError::Io {
                path: path.display().to_string(),
                source,
            })?;
        if n == 0 {
            return Ok(None);
        }
        let line = first.trim();
        if line.is_empty() {
            continue;
        }
        let start: DispatchStart =
            serde_json::from_str(line).map_err(|source| LoadError::Json {
                path: path.display().to_string(),
                line: 1,
                source,
            })?;
        // Prefer explicit parent_conversation; correlation_id defaults to the file stem when
        // missing so a bare start still keys.
        let child = start
            .correlation_id
            .or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .filter(|s| !s.is_empty());
        let parent = start.parent_conversation.filter(|s| !s.is_empty());
        return Ok(match (child, parent) {
            (Some(c), Some(p)) if c != p => Some((c, p)),
            _ => None,
        });
    }
}

/// Pure helper: build the map from already-parsed (child, parent) pairs (tests).
pub fn child_to_parent_map<I>(pairs: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    pairs.into_iter().collect()
}
