use chrono::{DateTime, Duration, NaiveDate, Utc};

use crate::completion_diff::TaskCounts;
use crate::completion_time::{PRECISION_TAP, SOURCE_LIBERADO};
use crate::completions::{CompletionDraft, NewCompletion, VoidDraft, liberado_tap_completion};
use crate::resolution::RES_COMPLETED;
use crate::rows::{EnqueueOutcome, NewReminder};
use crate::source::OutboxSource;
use crate::store::Outbox;

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("stamp")
        .with_timezone(&Utc)
}

fn db() -> (tempfile::TempDir, Outbox) {
    let dir = tempfile::tempdir().expect("dir");
    let outbox = Outbox::open(&dir.path().join("liberado.db")).expect("db");
    (dir, outbox)
}

fn tap(id: Option<i64>, when: DateTime<Utc>) -> NewCompletion {
    let mut row = liberado_tap_completion("Home.md", "Call dentist", id.unwrap_or(1), when, "UTC");
    row.outbox_id = id;
    row
}

fn reminder(key: &str, path: &str, text: &str, when: DateTime<Utc>) -> NewReminder {
    NewReminder {
        idempotency_key: key.into(),
        channel: "webpush".into(),
        target: "sub:1".into(),
        title: "Due".into(),
        body: "body".into(),
        due_at: when,
        catch_up: false,
        kind: Some("task_due".into()),
        source: Some(OutboxSource::VaultNote {
            path: path.into(),
            label: "due".into(),
            text: text.into(),
        }),
    }
}

fn resolve(outbox: &Outbox, key: &str, path: &str, text: &str, when: DateTime<Utc>) -> i64 {
    let EnqueueOutcome::Inserted(id) = outbox
        .enqueue(&reminder(key, path, text, when), when)
        .expect("enqueue")
    else {
        panic!("duplicate reminder");
    };
    outbox
        .try_resolve(id, RES_COMPLETED, when)
        .expect("resolve");
    id
}

#[test]
fn a_repeated_outbox_id_inserts_one_row_and_null_ids_do_not_collide() {
    let (_dir, outbox) = db();
    let when = at("2026-11-10T15:00:00Z");
    let row = tap(Some(4), when);
    assert!(outbox.insert_completion(&row).expect("first"));
    assert!(!outbox.insert_completion(&row).expect("second"));
    assert!(outbox.insert_completion(&tap(None, when)).expect("null a"));
    assert!(outbox.insert_completion(&tap(None, when)).expect("null b"));
    assert_eq!(outbox.list_completions(None, 10).expect("list").len(), 3);
    let stored = &outbox.list_completions(None, 10).expect("list")[0];
    assert_eq!(stored.precision, PRECISION_TAP);
    assert_eq!(stored.source, SOURCE_LIBERADO);
    assert_eq!(stored.outbox_id, Some(4));
}

#[test]
fn a_hint_inside_fifteen_minutes_matches_and_an_older_one_does_not() {
    let (_dir, outbox) = db();
    let detected = at("2026-11-10T15:00:00Z");
    let recent = detected - Duration::minutes(15);
    let stale = detected - Duration::minutes(15) - Duration::seconds(1);
    let recent_id = resolve(&outbox, "recent", "Home.md", "Call dentist", recent);
    resolve(&outbox, "stale", "Other.md", "Call dentist", stale);
    let hint = outbox
        .find_done_hint("Home.md", "Call dentist", detected)
        .expect("hint");
    let hint = hint.expect("recent hint");
    assert_eq!(hint.outbox_id, recent_id);
    assert_eq!(hint.resolved_at, recent);
    assert!(
        outbox
            .find_done_hint("Other.md", "Call dentist", detected)
            .expect("stale lookup")
            .is_none()
    );
}

#[test]
fn a_used_outbox_id_is_not_offered_again() {
    let (_dir, outbox) = db();
    let detected = at("2026-11-10T15:00:00Z");
    let id = resolve(&outbox, "once", "Home.md", "Call dentist", detected);
    let row = tap(Some(id), detected);
    assert!(outbox.insert_completion(&row).expect("insert"));
    assert!(
        outbox
            .find_done_hint("Home.md", "Call dentist", detected)
            .expect("lookup")
            .is_none()
    );
}

#[test]
fn prune_deletes_old_outbox_rows_and_leaves_completions() {
    let (_dir, outbox) = db();
    let when = at("2026-11-10T15:00:00Z");
    let EnqueueOutcome::Inserted(id) = outbox
        .enqueue(&reminder("old", "Home.md", "Call", when), when)
        .expect("enqueue")
    else {
        panic!("duplicate");
    };
    outbox
        .mark_accepted(&[id], when - Duration::days(31))
        .expect("accept");
    assert!(outbox.insert_completion(&tap(None, when)).expect("log"));
    assert_eq!(outbox.prune(when).expect("prune"), 1);
    assert_eq!(outbox.list_completions(None, 10).expect("list").len(), 1);
}

#[test]
fn list_since_keeps_rows_at_or_after_the_cutoff() {
    let (_dir, outbox) = db();
    let early = at("2026-11-09T12:00:00Z");
    let late = at("2026-11-10T12:00:00Z");
    assert!(outbox.insert_completion(&tap(None, early)).expect("early"));
    assert!(outbox.insert_completion(&tap(None, late)).expect("late"));
    let rows = outbox
        .list_completions(Some("2026-11-10T12:00:00Z"), 10)
        .expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].completed_at, "2026-11-10T12:00:00Z");
}

#[test]
fn apply_note_writes_the_snapshot_and_a_later_void_marks_the_row() {
    let (_dir, outbox) = db();
    let detected = at("2026-11-10T15:00:00Z");
    let mut counts = std::collections::BTreeMap::new();
    counts.insert("Call dentist".into(), TaskCounts { open: 0, closed: 1 });
    let draft = CompletionDraft {
        task_text: "Call dentist".into(),
        count: 1,
        prior_closed: 0,
        hint_text: None,
        done_date: Some(NaiveDate::from_ymd_opt(2026, 11, 9).expect("date")),
    };
    outbox
        .apply_completion_note(&crate::completions::NoteApply {
            path: "Home.md",
            fingerprint: "fp-1",
            counts: &counts,
            completions: &[draft],
            voids: &[],
            detected_at: detected,
            zone: "UTC",
        })
        .expect("apply");
    let saved = outbox.snapshot_counts("Home.md").expect("snapshot");
    assert_eq!(saved.get("Call dentist").map(|row| row.closed), Some(1));
    let rows = outbox.list_completions(None, 10).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].precision, "date");
    assert!(rows[0].voided_at.is_none());

    let void = VoidDraft {
        task_text: "Call dentist".into(),
        count: 1,
    };
    outbox
        .apply_completion_note(&crate::completions::NoteApply {
            path: "Home.md",
            fingerprint: "fp-2",
            counts: &counts,
            completions: &[],
            voids: &[void],
            detected_at: detected,
            zone: "UTC",
        })
        .expect("void");
    let rows = outbox.list_completions(None, 10).expect("list");
    assert_eq!(rows.len(), 1);
    assert!(rows[0].voided_at.is_some());
}
