use super::TaskStore;
use liberado_delegate_contract::{Acceptance, TaskBudget, TaskGrant, TaskId, TaskSpec, TaskStatus};

fn spec(id: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId(id.to_string()),
        project: "liberado".into(),
        repository: "shiloh/liberado".into(),
        base_branch: "main".into(),
        goal: "add the thing".into(),
        success_criteria: vec!["tests pass".into()],
        acceptance: Acceptance::default(),
        budget: TaskBudget::default(),
        grant: TaskGrant::default(),
    }
}

fn store() -> (tempfile::TempDir, TaskStore) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TaskStore::open(tmp.path()).expect("store opens");
    (tmp, store)
}

#[test]
fn submit_then_resubmit_is_a_duplicate_no_op() {
    let (_tmp, store) = store();
    let first = store.submit(&spec("01ARZ3NDEK")).expect("submit");
    assert!(!first.duplicate);
    assert_eq!(first.record.status, TaskStatus::Queued);

    let second = store.submit(&spec("01ARZ3NDEK")).expect("resubmit");
    assert!(second.duplicate, "same id must not re-run");
    assert_eq!(second.record.spec.goal, first.record.spec.goal);
}

/// A duplicate redelivery must return the *current* record — if a poller replays an old
/// submit after the task finished, it must see PrOpened, not a stale Queued.
#[test]
fn resubmit_returns_the_current_record_not_a_stale_copy() {
    let (_tmp, store) = store();
    let id = TaskId("01ARZ3NDEKTSV4".into());
    store.submit(&spec(&id.0)).expect("submit");
    store
        .finish(
            &id,
            TaskStatus::PrOpened {
                url: "http://forge/pr/1".into(),
            },
        )
        .expect("finish");

    let replay = store.submit(&spec(&id.0)).expect("resubmit");
    assert!(replay.duplicate);
    assert_eq!(
        replay.record.status,
        TaskStatus::PrOpened {
            url: "http://forge/pr/1".into()
        }
    );
}

#[test]
fn finish_lifts_pr_url_out_of_the_variant() {
    let (_tmp, store) = store();
    let id = TaskId("01ARZ3NDEKT99".into());
    store.submit(&spec(&id.0)).expect("submit");

    let record = store
        .finish(
            &id,
            TaskStatus::PrOpened {
                url: "http://forge/pr/9".into(),
            },
        )
        .expect("finish");
    assert_eq!(record.pr_url.as_deref(), Some("http://forge/pr/9"));
    assert_eq!(
        record.status,
        TaskStatus::PrOpened {
            url: "http://forge/pr/9".into()
        }
    );

    let reloaded = store.get(&id.0).expect("get").expect("exists");
    assert_eq!(reloaded.pr_url.as_deref(), Some("http://forge/pr/9"));
}

#[test]
fn failed_status_keeps_pr_url_empty() {
    let (_tmp, store) = store();
    let id = TaskId("01ARZ3NDEKFA".into());
    store.submit(&spec(&id.0)).expect("submit");
    let record = store
        .finish(
            &id,
            TaskStatus::Failed {
                reason: "compile red".into(),
            },
        )
        .expect("finish");
    assert_eq!(record.pr_url, None);
}

#[test]
fn mark_running_records_the_session_id() {
    let (_tmp, store) = store();
    let id = TaskId("01ARZ3NDEKRU".into());
    store.submit(&spec(&id.0)).expect("submit");
    let record = store.mark_running(&id, "session-7").expect("running");
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(record.session_id.as_deref(), Some("session-7"));
}

#[test]
fn cancel_is_queued_ok_terminal_idempotent_running_refused() {
    let (_tmp, store) = store();

    // Unknown task.
    match store.cancel("no-such-task") {
        Err(super::CancelError::NotFound(id)) => assert_eq!(id, "no-such-task"),
        other => panic!("expected NotFound, got {other:?}"),
    }

    // Queued cancels cleanly.
    store.submit(&spec("01ARZ3NDEKQ1")).expect("submit queued");
    let cancelled = store.cancel("01ARZ3NDEKQ1").expect("cancel queued");
    assert_eq!(cancelled.status, TaskStatus::Cancelled);

    // Terminal states are idempotent no-ops.
    let again = store.cancel("01ARZ3NDEKQ1").expect("cancel terminal");
    assert_eq!(again.status, TaskStatus::Cancelled);

    // Running refuses.
    let id = TaskId("01ARZ3NDEKR2".into());
    store.submit(&spec(&id.0)).expect("submit running");
    store.mark_running(&id, "s").expect("mark running");
    match store.cancel(&id.0) {
        Err(super::CancelError::Running(running)) => assert_eq!(running, id.0),
        other => panic!("expected Running refusal, got {other:?}"),
    }
}

#[test]
fn known_ids_lists_every_submitted_task() {
    let (_tmp, store) = store();
    store.submit(&spec("task-b")).expect("submit b");
    store.submit(&spec("task-a")).expect("submit a");
    assert_eq!(store.known_ids().expect("ids"), vec!["task-a", "task-b"]);
}

#[test]
fn records_survive_reopening_the_store() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let id;
    {
        let store = TaskStore::open(tmp.path()).expect("store");
        let submitted = store.submit(&spec("01ARZ3NDEKPERSIST")).expect("submit");
        id = submitted.record.spec.id.clone();
        store.mark_running(&id, "sess").expect("running");
    }
    let reopened = TaskStore::open(tmp.path()).expect("reopen");
    let record = reopened.get(&id.0).expect("get").expect("survives");
    assert_eq!(record.session_id.as_deref(), Some("sess"));
}

// --- the event journal ----------------------------------------------------

use liberado_delegate_contract::{EventKind, WorkerEvent};

fn events_dir(tmp: &tempfile::TempDir, id: &str) -> std::path::PathBuf {
    tmp.path().join("delegate").join("tasks").join(id)
}

#[test]
fn every_transition_appends_a_durable_event_with_increasing_correlations() {
    let (tmp, store) = store();
    let id = TaskId("01JEVSEQ0000000000A".into());
    let task_id = id.clone();
    store.submit(&spec(&id.0)).expect("submit");
    store.mark_running(&id, "s1").expect("running");
    store
        .finish(
            &id,
            TaskStatus::PrOpened {
                url: "http://forge/pr/3".into(),
            },
        )
        .expect("finish");

    let journal = events_dir(&tmp, &id.0).join("events.jsonl");
    assert!(journal.exists(), "journal must be on disk");
    let raw = std::fs::read_to_string(journal).expect("journal readable");
    let events: Vec<WorkerEvent> = raw
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line parses"))
        .collect();

    let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            EventKind::StatusChanged,
            EventKind::StatusChanged,
            EventKind::PrReady
        ],
        "queued -> running -> pr_ready"
    );
    assert_eq!(events[2].payload["status"]["state"], "pr_opened");
    assert_eq!(
        events[2].payload["status"]["detail"]["url"],
        "http://forge/pr/3"
    );

    // Correlations carry the persisted monotonic sequence, so replays and dedupe stay
    // stable across restarts.
    let seqs: Vec<&str> = events
        .iter()
        .map(|e| e.correlation_id.split(':').next_back().unwrap())
        .collect();
    assert_eq!(seqs, vec!["1", "2", "3"], "{events:?}");

    // Replay from a fresh handle returns exactly what the journal holds.
    let reopened = TaskStore::open(tmp.path()).expect("reopen");
    assert_eq!(reopened.replay(&task_id.0).expect("replay"), events);
}

#[test]
fn a_duplicate_submit_records_no_event() {
    let (_tmp, store) = store();
    store.submit(&spec("01JEVSEQ0000000000B")).expect("first");
    store
        .submit(&spec("01JEVSEQ0000000000B"))
        .expect("duplicate");
    assert_eq!(
        store.replay("01JEVSEQ0000000000B").expect("replay").len(),
        1,
        "redelivery is a no-op and must stay silent"
    );
}

#[test]
fn cancel_and_failure_emit_terminal_status_events() {
    let (_tmp, store) = store();
    store.submit(&spec("01JEVSEQ0000000000C")).expect("submit");
    store.cancel("01JEVSEQ0000000000C").expect("cancel");
    let cancelled = store.replay("01JEVSEQ0000000000C").expect("replay");
    assert_eq!(
        cancelled.last().expect("an event").kind,
        EventKind::StatusChanged
    );
    assert!(
        cancelled.last().unwrap().is_terminal(),
        "cancelled closes the story"
    );

    let id = TaskId("01JEVSEQ0000000000D".into());
    store.submit(&spec(&id.0)).expect("submit d");
    store
        .finish(
            &id,
            TaskStatus::Failed {
                reason: "boom".into(),
            },
        )
        .expect("fail");
    let failed = store.replay(&id.0).expect("replay");
    assert!(failed.last().unwrap().is_terminal());
}

#[test]
fn terminal_pr_opened_maps_to_pr_ready_with_url() {
    use super::event_shape;
    use liberado_delegate_contract::EventKind;
    let (kind, url) = event_shape(&TaskStatus::PrOpened { url: "u".into() });
    assert_eq!(kind, EventKind::PrReady);
    assert_eq!(url.as_deref(), Some("u"));
    let (kind, url) = event_shape(&TaskStatus::Failed { reason: "r".into() });
    assert_eq!(kind, EventKind::StatusChanged);
    assert_eq!(url, None);
}
