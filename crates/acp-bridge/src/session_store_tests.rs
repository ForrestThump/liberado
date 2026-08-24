//! Split from `session_store.rs` for module-health boundaries.

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

#[test]
fn validate_id_rejects_each_separator_on_its_own() {
    for bad in ["a/b", "a\\b", "a\u{0}b"] {
        assert!(validate_id(bad).is_err(), "{bad:?} must be rejected");
    }
    assert!(validate_id("plain-id").is_ok());
}

#[test]
fn load_reports_corrupt_records_as_errors_not_as_absent() {
    let dir = TempDir::new().unwrap();
    let (_lock, _guard) = set_sessions_dir(&dir);
    let id = "corrupt";
    std::fs::write(sessions_dir().join(format!("{id}.json")), "{ not json }").unwrap();
    let err = load(id).expect_err("a corrupt record must surface, not read as absent");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn updates_stamp_rfc3339_timestamps() {
    let dir = TempDir::new().unwrap();
    {
        let (_lock, _g) = set_sessions_dir(&dir);
        save(&sample_record("stamp")).unwrap();
        update_model("stamp", "m2").unwrap();
        let loaded = load("stamp").unwrap().unwrap();
        assert!(
            chrono::DateTime::parse_from_rfc3339(&loaded.updated_at).is_ok(),
            "updated_at must stay RFC3339, got {:?}",
            loaded.updated_at
        );
    }
}
