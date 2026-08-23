//! Survivor tests for the dispatch journal (face → dispatcher delegation bookends).
//!
//! `LIBERADO_DATA_DIR` is process-global and every `cargo test` binary in this crate runs its
//! tests concurrently, so everything that touches it serializes on one lock — including the
//! *clearing* half of each mutation (the window between a set and a clear is what bit
//! `coder-sandbox::checkpoint`; see AGENTS.md). The async tests hold a `tokio` Mutex across
//! awaits, which is both sound and clippy-clean where a std guard would be neither.

use super::*;
use serde_json::Value;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

async fn env_lock_async() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

fn set_data_dir(path: &std::path::Path) {
    // Edition-2024 makes these unsafe; the locks above are what make them sound in tests.
    unsafe { std::env::set_var("LIBERADO_DATA_DIR", path) };
}

fn clear_data_dir() {
    unsafe { std::env::remove_var("LIBERADO_DATA_DIR") };
}

#[test]
fn journal_paths_follow_the_data_dir() {
    let _g = env_lock();
    let dir = tempfile::tempdir().unwrap();
    set_data_dir(dir.path());

    assert_eq!(dispatches_dir(), dir.path().join("dispatches"));
    assert_eq!(
        journal_path("chat-x"),
        dir.path().join("dispatches").join("chat-x.jsonl")
    );

    clear_data_dir();
    assert_eq!(
        dispatches_dir(),
        PathBuf::from(".liberado").join("dispatches")
    );
}

#[tokio::test]
async fn append_writes_one_jsonl_line() {
    let _g = env_lock_async().await;
    let dir = tempfile::tempdir().unwrap();
    set_data_dir(dir.path());

    append("c1", serde_json::json!({ "kind": "start", "n": 1 })).await;

    let file = journal_path("c1");
    let body = tokio::fs::read_to_string(&file)
        .await
        .expect("journal written");
    let mut lines = body.lines();
    let first: Value = serde_json::from_str(lines.next().expect("one line")).unwrap();
    assert_eq!(first["kind"], "start");
    assert_eq!(first["n"], 1);
    assert!(
        lines.next().is_none(),
        "exactly one line, newline-terminated"
    );
    assert!(body.ends_with('\n'));

    clear_data_dir();
}

#[tokio::test]
async fn append_abandons_when_the_parent_cannot_be_created() {
    let _g = env_lock_async().await;
    let dir = tempfile::tempdir().unwrap();
    // The data-dir "path" is an existing FILE: create_dir_all must fail, and append must give
    // up without writing anything anywhere.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    set_data_dir(&blocker);

    append("c2", serde_json::json!({ "kind": "start" })).await;

    assert!(!blocker.join("dispatches").exists());
    assert!(!journal_path("c2").exists());

    clear_data_dir();
}

#[test]
fn start_record_carries_the_bookend_fields() {
    let rec = start_record(
        "chat-d1",
        Some("parent-chat"),
        "water the plants",
        Some("m1"),
    );
    assert_eq!(rec["kind"], "start");
    assert_eq!(rec["correlation_id"], "chat-d1");
    assert_eq!(rec["parent_conversation"], "parent-chat");
    assert_eq!(rec["goal"], "water the plants");
    assert_eq!(rec["model"], "m1");
    let journal = rec["journal"].as_str().expect("journal path recorded");
    assert!(
        journal.ends_with("chat-d1.jsonl"),
        "journal field names this delegation's file: {journal}"
    );
    assert!(rec["ts"].as_str().is_some_and(|t| !t.is_empty()));
}

#[test]
fn disposition_record_carries_summary_and_model() {
    let rec = disposition_record("done: 3 tasks", Some("m2"));
    assert_eq!(rec["kind"], "disposition");
    assert_eq!(rec["summary"], "done: 3 tasks");
    assert_eq!(rec["model"], "m2");
    assert!(rec["ts"].as_str().is_some_and(|t| !t.is_empty()));
}

#[test]
fn display_path_is_the_repo_relative_hint() {
    // Built from Path joins so the assertion holds under Windows separators too.
    let expected = Path::new(".liberado")
        .join("dispatches")
        .join("c9.jsonl")
        .display()
        .to_string();
    assert_eq!(journal_display_path("c9"), expected);
}
