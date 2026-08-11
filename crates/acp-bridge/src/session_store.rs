//! File-backed session records for the ACP bridge.
//!
//! Sessions are stored as JSON files under `<LIBERADO_DATA_DIR>/acp-sessions/`,
//! falling back to `.liberado/acp-sessions/`. Each write is atomic (temp file +
//! rename). Session ids are treated as untrusted and sanitized before use as
//! filenames.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

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
/// Tests may call [`set_test_sessions_dir`] to override the directory globally
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
/// Point `sessions_dir()` at a temp directory for the duration of this test.
///
/// The returned guard holds the lock and restores the default on drop, so the redirection and
/// the exclusion have exactly the same lifetime — a caller cannot hold one without the other.
///
/// Serializes every test that redirects `sessions_dir()`. `TEST_SESSIONS_DIR` is process-global,
/// and `cargo test` runs a binary's tests concurrently on one process. Without this lock each
/// test overwrites the directory the others are using.
pub(crate) fn set_sessions_dir(
    dir: &TempDir,
) -> (std::sync::MutexGuard<'static, ()>, TestDirGuard) {
    // A poisoned lock here means another test panicked while holding it. The directory is
    // restored by `TestDirGuard`'s Drop during that unwind, so the state is still sound and
    // failing every subsequent test on it would hide the one real failure.
    let lock = DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    (lock, set_test_sessions_dir(dir.path().to_path_buf()))
}

#[cfg(test)]
static DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn set_test_sessions_dir(dir: PathBuf) -> TestDirGuard {
    *TEST_SESSIONS_DIR.lock().expect("lock") = Some(dir);
    TestDirGuard
}

#[cfg(test)]
pub(crate) struct TestDirGuard;

#[cfg(test)]
impl Drop for TestDirGuard {
    fn drop(&mut self) {
        *TEST_SESSIONS_DIR.lock().expect("lock") = None;
    }
}

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

/// Confirm `resolved` lives under the sessions directory — defense-in-depth
/// on top of [`validate_id`].
fn within_dir(resolved: &Path) -> bool {
    resolved.parent() == Some(sessions_dir().as_path())
}

// ── Public API ──────────────────────────────────────────────────────────────

/// Save a session record atomically.
///
/// Writes to a temporary file in the same directory, then renames over the
/// target so a crash mid-write cannot leave a half-written record that would
/// later parse as truth.
pub fn save(record: &SessionRecord) -> io::Result<()> {
    let dir = sessions_dir();
    std::fs::create_dir_all(&dir)?;

    let target = record_path(&record.id)?;
    if !within_dir(&target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "session id {:?} resolves outside sessions directory",
                record.id
            ),
        ));
    }

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
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_record(id: &str) -> SessionRecord {
        SessionRecord {
            id: id.to_string(),
            mode: "coding".into(),
            cwd: PathBuf::from("/home/user/project"),
            model: "deepseek-v4-pro".into(),
            messages: vec![],
            updated_at: "2025-01-01T00:00:00Z".into(),
        }
    }

    // ── Round-trip ──────────────────────────────────────────────────────

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let record = sample_record("lib-abc123");
        save(&record).expect("save must succeed");
        let loaded = load("lib-abc123")
            .expect("load must succeed")
            .expect("record must be present");
        assert_eq!(loaded, record, "loaded record must equal the saved one");
    }

    // ── Not found ───────────────────────────────────────────────────────

    #[test]
    fn load_never_saved_id_is_clean_none() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let result = load("does-not-exist").expect("load must not error");
        assert!(
            result.is_none(),
            "a never-saved id must be None, not a panic or an error"
        );
    }

    // ── Path traversal ──────────────────────────────────────────────────

    #[test]
    fn path_separator_in_id_cannot_write_outside_directory() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        // An id containing a path separator — treated as untrusted.
        let record = sample_record("../../../etc/passwd");
        let err = save(&record).expect_err("must refuse a traversal id");
        let msg = err.to_string();
        assert!(
            msg.contains("path separator"),
            "error must mention path separator, got: {msg}"
        );
    }

    #[test]
    fn dot_dot_in_id_cannot_write_outside_directory() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        // Raw `..` is a path traversal component and must be rejected.
        let record = sample_record("..");
        let err = save(&record).expect_err("a raw-dot-dot id must not escape");
        let msg = err.to_string();
        assert!(
            msg.contains("path traversal"),
            "error must mention traversal, got: {msg}"
        );
        // `../something` is caught by the path separator check.
        let record2 = sample_record("../etc/passwd");
        let err2 = save(&record2).expect_err("dot-dot-slash must be rejected");
        assert!(
            err2.to_string().contains("path separator"),
            "error must mention path separator, got: {}",
            err2
        );
    }

    // ── validate_id rejects empty / uninformative ids ─────────────────────

    #[test]
    fn empty_id_is_rejected() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let record = sample_record("");
        let err = save(&record).expect_err("empty id must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must not be empty"),
            "error must mention empty, got: {msg}"
        );
    }

    #[test]
    fn dots_only_id_is_rejected() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        // "...hidden" starts with dots, passes path-sep check, and is not "." or "..",
        // so it is a valid (if odd) id. But "..." by itself is neither "." nor ".."
        // exactly, so it is accepted as a valid filename component.
        let record = sample_record("...");
        save(&record).expect("three-dot id is not .. exactly, so it is valid");

        // "...." likewise — not a traversal.
        let record2 = sample_record("....");
        save(&record2).expect("four-dot id is valid");

        // "." IS rejected as a path traversal.
        let record3 = sample_record(".");
        let err = save(&record3).expect_err("bare dot must be rejected");
        assert!(
            err.to_string().contains("path traversal"),
            "error for '.' must mention traversal, got: {}",
            err
        );
    }

    #[test]
    fn ids_differing_only_in_leading_dots_do_not_collide() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);

        // ids that differ only in leading dots must produce different files.
        save(&sample_record("..hidden")).expect("save ..hidden");
        save(&sample_record(".hidden")).expect("save .hidden");
        save(&sample_record("hidden")).expect("save hidden");

        let r1 = load("..hidden").expect("load").expect("present");
        let r2 = load(".hidden").expect("load").expect("present");
        let r3 = load("hidden").expect("load").expect("present");

        assert_eq!(r1.id, "..hidden");
        assert_eq!(r2.id, ".hidden");
        assert_eq!(r3.id, "hidden");
        // All three must be distinct records.
        let sd = sessions_dir();
        let json_files: Vec<String> = std::fs::read_dir(&sd)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        assert_eq!(
            json_files.len(),
            3,
            "three distinct ids must yield three files, got {json_files:?}"
        );
    }

    // ── No leftover temp file ───────────────────────────────────────────

    #[test]
    fn replacing_a_record_leaves_no_temporary_file_behind() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);

        let mut record = sample_record("lib-replace");
        save(&record).expect("first save");

        record.mode = "chat".into();
        save(&record).expect("second save (overwrite)");

        // The sessions directory must contain exactly one file (the record,
        // not the temp).
        let sd = sessions_dir();
        let entries: Vec<String> = std::fs::read_dir(&sd)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "exactly one file expected, got {entries:?}"
        );
        let name = &entries[0];
        assert!(
            !name.ends_with(".tmp"),
            "no .tmp file must remain after rename, got {name}"
        );
        assert!(
            name.ends_with(".json"),
            "the surviving file must be the record, got {name}"
        );
    }

    // ── Append messages ─────────────────────────────────────────────────

    #[test]
    fn append_messages_adds_user_and_assistant() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let record = sample_record("lib-append");
        save(&record).expect("save");

        append_messages("lib-append", "hello", "hi there").expect("append");

        let loaded = load("lib-append").expect("load").expect("present");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[0].content, "hello");
        assert_eq!(loaded.messages[1].role, "assistant");
        assert_eq!(loaded.messages[1].content, "hi there");
        // updated_at must have advanced from the fixture timestamp.
        assert_ne!(loaded.updated_at, record.updated_at);
    }

    #[test]
    fn append_messages_on_missing_record_is_noop() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        // Must not panic, must not create a file.
        append_messages("nope", "hello", "world").expect("no error");
        assert!(load("nope").expect("load").is_none());
    }

    #[test]
    fn empty_messages_are_skipped() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let record = sample_record("lib-empty-msg");
        save(&record).expect("save");

        append_messages("lib-empty-msg", "", "").expect("append empty");
        let loaded = load("lib-empty-msg").expect("load").expect("present");
        assert!(
            loaded.messages.is_empty(),
            "empty messages must not be appended"
        );
    }

    // ── Update helpers ──────────────────────────────────────────────────

    #[test]
    fn update_mode_changes_the_field() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let record = sample_record("lib-mode");
        save(&record).expect("save");

        update_mode("lib-mode", "chat").expect("update_mode");
        let loaded = load("lib-mode").expect("load").expect("present");
        assert_eq!(loaded.mode, "chat");
    }

    #[test]
    fn update_model_changes_the_field() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let record = sample_record("lib-model");
        save(&record).expect("save");

        update_model("lib-model", "gpt-4o").expect("update_model");
        let loaded = load("lib-model").expect("load").expect("present");
        assert_eq!(loaded.model, "gpt-4o");
    }

    // ── Append preserves existing transcript ────────────────────────────

    #[test]
    fn append_messages_preserves_existing_transcript() {
        let dir = TempDir::new().unwrap();
        let _guards = set_sessions_dir(&dir);
        let mut record = sample_record("lib-append-preserve");
        record.messages = vec![
            StoredMessage {
                role: "user".into(),
                content: "first question".into(),
            },
            StoredMessage {
                role: "assistant".into(),
                content: "first answer".into(),
            },
        ];
        save(&record).expect("save");

        append_messages("lib-append-preserve", "second question", "second answer").expect("append");

        let loaded = load("lib-append-preserve").expect("load").expect("present");
        assert_eq!(
            loaded.messages.len(),
            4,
            "must preserve prior messages and append new ones"
        );
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[0].content, "first question");
        assert_eq!(loaded.messages[1].role, "assistant");
        assert_eq!(loaded.messages[1].content, "first answer");
        assert_eq!(loaded.messages[2].role, "user");
        assert_eq!(loaded.messages[2].content, "second question");
        assert_eq!(loaded.messages[3].role, "assistant");
        assert_eq!(loaded.messages[3].content, "second answer");
    }
}
