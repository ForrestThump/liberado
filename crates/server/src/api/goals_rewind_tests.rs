//! Split from `goals.rs` for module-health boundaries.

use super::*;
use liberado_session::{SessionEvent, SessionEventKind};

fn checkpoint(id: &str, label: &str, hash: &str) -> SessionEvent {
    SessionEvent {
        session_id: "s1".into(),
        at: chrono::Utc::now(),
        kind: SessionEventKind::Checkpoint {
            id: id.into(),
            label: label.into(),
            tree_hash: hash.into(),
        },
    }
}

fn token_event() -> SessionEvent {
    SessionEvent {
        session_id: "s1".into(),
        at: chrono::Utc::now(),
        kind: SessionEventKind::Token { text: "hi".into() },
    }
}

#[test]
fn rewind_workspace_prefers_an_existing_durable_dir() {
    let durable = tempfile::tempdir().unwrap();
    let got = rewind_workspace(Some(durable.path().to_path_buf()), Some("/payload")).unwrap();
    assert_eq!(got, durable.path());
}

#[test]
fn rewind_workspace_falls_back_to_payload_when_durable_is_missing() {
    let gone = std::path::PathBuf::from("C:\\definitely-not-here-rewind-test");
    let got = rewind_workspace(Some(gone), Some("/payload")).unwrap();
    assert_eq!(got, std::path::PathBuf::from("/payload"));
}

#[test]
fn rewind_workspace_uses_payload_when_no_durable_dir_exists() {
    let got = rewind_workspace(None, Some("/payload")).unwrap();
    assert_eq!(got, std::path::PathBuf::from("/payload"));
}

#[test]
fn rewind_workspace_reports_both_missing_sources() {
    let err = rewind_workspace(None, None).unwrap_err();
    assert!(err.contains("no workspace_root in payload"), "{err}");
    let gone = std::path::PathBuf::from("C:\\definitely-not-here-rewind-test");
    let err = rewind_workspace(Some(gone), None).unwrap_err();
    assert!(err.contains("no durable session worktree"), "{err}");
}

#[test]
fn rewind_checkpoint_explicit_id_wins_with_event_label() {
    let events = vec![
        checkpoint("c1", "first", "h1"),
        token_event(),
        checkpoint("c2", "second", "h2"),
    ];
    let got = rewind_checkpoint(&events, Some("c1")).unwrap();
    assert_eq!(got, ("c1".into(), "first".into(), "h1".into()));
}

#[test]
fn rewind_checkpoint_unknown_explicit_id_falls_back_to_explicit_label() {
    let events = vec![checkpoint("c1", "first", "h1")];
    let got = rewind_checkpoint(&events, Some("nope")).unwrap();
    assert_eq!(got, ("nope".into(), "explicit".into(), String::new()));
}

#[test]
fn rewind_checkpoint_no_id_uses_the_most_recent_checkpoint() {
    let events = vec![
        checkpoint("c1", "first", "h1"),
        token_event(),
        checkpoint("c2", "second", "h2"),
    ];
    let got = rewind_checkpoint(&events, None).unwrap();
    assert_eq!(got, ("c2".into(), "second".into(), "h2".into()));
}

#[test]
fn rewind_checkpoint_no_checkpoints_errors() {
    let err = rewind_checkpoint(&[token_event()], None).unwrap_err();
    assert!(err.contains("no checkpoint events"), "{err}");
}
