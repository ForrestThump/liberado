//! File-backed session records for the ACP bridge.
//!
//! Sessions are stored as JSON files under `<LIBERADO_DATA_DIR>/acp-sessions/`,
//! falling back to `.liberado/acp-sessions/`. Each write is atomic (temp file +
//! rename). Session ids are treated as untrusted and sanitized before use as
//! filenames.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[cfg(test)]
use tempfile::TempDir;

// ── Types ───────────────────────────────────────────────────────────────────

/// One message in a session transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
}

/// A serialisable session record persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub mode: String,
    pub cwd: PathBuf,
    pub model: String,
    pub messages: Vec<StoredMessage>,
    pub updated_at: String,
}

// ── Directory resolution ────────────────────────────────────────────────────

/// Directory for ACP session records.
///
/// `<LIBERADO_DATA_DIR>/acp-sessions/` when the env var is set; otherwise
/// `.liberado/acp-sessions/` relative to the working directory — the same
/// fallback `crates/config/src/lib.rs::data_dir()` and
/// `crates/acp-bridge/src/coding_run.rs` use.
///
/// Tests may call `set_sessions_dir` to override the directory globally
/// (avoids env-var races under parallel `cargo test`).
pub fn sessions_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(d) = TEST_SESSIONS_DIR.lock().ok().and_then(|g| g.clone()) {
        return d;
    }
    let base = std::env::var("LIBERADO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".liberado"));
    base.join("acp-sessions")
}

#[cfg(test)]
static TEST_SESSIONS_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

#[cfg(test)]
/// Holds `sessions_dir()` redirected for the duration of one test.
///
/// Owns the directory lock, and the reset runs in [`Drop`](Self::drop) **while that lock is
/// still held** — a struct's fields release only after its `Drop::drop` returns, so the
/// redirection and its teardown are atomic with respect to every other test in the binary.
/// The previous two-value tuple returned `(MutexGuard, TestDirGuard)` separately, and tuples
/// drop left-to-right: the lock released *before* the reset, leaving a window where a parallel
/// test could acquire the lock, set its own override, and have this guard's teardown clobber
/// it mid-flight (~1-in-3 full-binary runs).
pub(crate) struct SessionsDirOverride {
    _dir_lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for SessionsDirOverride {
    fn drop(&mut self) {
        *TEST_SESSIONS_DIR.lock().expect("lock") = None;
    }
}

#[cfg(test)]
/// Point `sessions_dir()` at a temp directory for the duration of this test.
///
/// Serializes every test that redirects `sessions_dir()`. `TEST_SESSIONS_DIR` is process-global,
/// and `cargo test` runs a binary's tests concurrently on one process. Without this lock each
/// test overwrites the directory the others are using.
pub(crate) fn set_sessions_dir(dir: &TempDir) -> SessionsDirOverride {
    // A poisoned lock here means another test panicked while holding it. The directory is
    // restored by `SessionsDirOverride`'s Drop during that unwind, so the state is still sound
    // and failing every subsequent test on it would hide the one real failure.
    let dir_lock = DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *TEST_SESSIONS_DIR.lock().expect("lock") = Some(dir.path().to_path_buf());
    SessionsDirOverride {
        _dir_lock: dir_lock,
    }
}

#[cfg(test)]
static DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── Path helpers ────────────────────────────────────────────────────────────

/// Validate a session id for use as a filename component.
///
/// Reject path separators, null bytes, bare `.` / `..`, and empty strings.
/// The id is treated as untrusted — it is returned unchanged when it passes
/// validation, never silently rewritten.
pub(crate) fn validate_id(id: &str) -> io::Result<&str> {
    if id.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session id must not be empty",
        ));
    }
    if id.contains('/') || id.contains('\\') || id.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("session id contains a path separator: {id:?}"),
        ));
    }
    // Bare `.` or `..` are path traversal components, not valid filenames.
    if id == ".." || id == "." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("session id is a path traversal: {id:?}"),
        ));
    }
    Ok(id)
}

/// Resolve the filesystem path for a session record, after validating the id.
fn record_path(id: &str) -> io::Result<PathBuf> {
    validate_id(id)?;
    Ok(sessions_dir().join(format!("{id}.json")))
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Save a session record atomically.
///
/// Writes to a temporary file in the same directory, then renames over the
/// target so a crash mid-write cannot leave a half-written record that would
/// later parse as truth.
///
/// Containment is structural: the sessions directory is read once and the
/// validated id is joined under it, so a record path cannot escape. (An earlier
/// version re-read `sessions_dir()` for a second containment check; with
/// redirection now race-free that re-read can never disagree, and its mismatch
/// branch was dead — validate_id remains the traversal boundary.)
pub fn save(record: &SessionRecord) -> io::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;

    let target = record_path(&record.id)?;

    let tmp = target.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(record)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    if let Err(e) = std::fs::rename(&tmp, &target) {
        // Don't leave a stale temp file behind.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Load a session record by id.
///
/// Returns `Ok(None)` when no record has been saved for this id — that is a
/// clean "not found", not an error.
pub fn load(id: &str) -> io::Result<Option<SessionRecord>> {
    let path = record_path(id)?;
    match std::fs::read_to_string(&path) {
        Ok(json) => {
            let record: SessionRecord = serde_json::from_str(&json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(record))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Append user and assistant messages to an existing session record.
///
/// If the record does not exist yet this is a no-op — the save from
/// `session/new` must have completed first. The caller logs the skip.
pub fn append_messages(id: &str, user_text: &str, assistant_text: &str) -> io::Result<()> {
    let mut record = match load(id)? {
        Some(r) => r,
        None => return Ok(()),
    };
    if !user_text.is_empty() {
        record.messages.push(StoredMessage {
            role: "user".into(),
            content: user_text.to_string(),
        });
    }
    if !assistant_text.is_empty() {
        record.messages.push(StoredMessage {
            role: "assistant".into(),
            content: assistant_text.to_string(),
        });
    }
    record.updated_at = new_timestamp();
    save(&record)
}

/// Update the `mode` field of an existing record.
///
/// No-op when the record has not been persisted yet.
pub fn update_mode(id: &str, mode: &str) -> io::Result<()> {
    let mut record = match load(id)? {
        Some(r) => r,
        None => return Ok(()),
    };
    record.mode = mode.to_string();
    record.updated_at = new_timestamp();
    save(&record)
}

/// Update the `model` field of an existing record.
///
/// No-op when the record has not been persisted yet.
pub fn update_model(id: &str, model: &str) -> io::Result<()> {
    let mut record = match load(id)? {
        Some(r) => r,
        None => return Ok(()),
    };
    record.model = model.to_string();
    record.updated_at = new_timestamp();
    save(&record)
}

// ── Internal helpers ────────────────────────────────────────────────────────

pub(crate) fn new_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "session_store_tests.rs"]
mod tests;
