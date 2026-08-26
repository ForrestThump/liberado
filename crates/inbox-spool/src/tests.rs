use super::{Appended, InboxItem, ItemKind, Spool, SpoolError};

fn spool() -> (tempfile::TempDir, Spool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path()).expect("open");
    (tmp, spool)
}

fn item(seq: u64, correlation: &str) -> InboxItem {
    InboxItem {
        seq,
        kind: ItemKind::Question,
        task_id: "01T".into(),
        correlation_id: correlation.into(),
        payload: serde_json::json!({}),
    }
}

#[test]
fn fresh_appends_get_monotonic_sequences() {
    let (_tmp, mut spool) = spool();
    assert_eq!(
        spool
            .append(ItemKind::PrReady, "01A", "c1", serde_json::json!({}))
            .unwrap(),
        Appended::Appended(1)
    );
    assert_eq!(
        spool
            .append(ItemKind::Question, "01A", "c2", serde_json::json!({}))
            .unwrap(),
        Appended::Appended(2)
    );
    let pending = spool.pending().unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].correlation_id, "c1");
    assert_eq!(pending[1].correlation_id, "c2");
}

/// The whole point of the correlation id: an SSE stream that redelivers, or an adapter
/// restarted mid-stream, must not enqueue the same question twice.
#[test]
fn a_replayed_correlation_is_a_duplicate_and_writes_nothing() {
    let (_tmp, mut spool) = spool();
    spool
        .append(
            ItemKind::Question,
            "01A",
            "delegate:01A:3",
            serde_json::json!({}),
        )
        .unwrap();

    let before = std::fs::read_to_string(spool.root.join("items.jsonl")).unwrap();
    let replay = spool
        .append(
            ItemKind::Question,
            "01A",
            "delegate:01A:3",
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(replay, Appended::Duplicate);
    let after = std::fs::read_to_string(spool.root.join("items.jsonl")).unwrap();
    assert_eq!(before, after, "a duplicate must not append bytes");
    assert_eq!(spool.pending().unwrap().len(), 1);
}

/// A crash between append and settle replays the item; settling is what removes it.
#[test]
fn settled_items_leave_the_pending_queue_but_stay_deduped() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = Spool::open(dir.path()).unwrap();
    first
        .append(
            ItemKind::Blocked,
            "01B",
            "delegate:01B:9",
            serde_json::json!({}),
        )
        .unwrap();
    first.settle(1).unwrap();

    // Reopen: state comes from disk, memory is gone.
    let mut reopened = Spool::open(dir.path()).unwrap();
    assert!(
        reopened.pending().unwrap().is_empty(),
        "settled stays settled"
    );
    let replay = reopened
        .append(
            ItemKind::Blocked,
            "01B",
            "delegate:01B:9",
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(replay, Appended::Duplicate);
    // Settling twice is a no-op.
    reopened.settle(1).unwrap();
}

#[test]
fn reopening_after_a_crash_resumes_the_sequence_without_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = Spool::open(dir.path()).unwrap();
    first
        .append(ItemKind::Note, "01C", "c1", serde_json::json!({}))
        .unwrap();
    first
        .append(ItemKind::Note, "01C", "c2", serde_json::json!({}))
        .unwrap();

    let mut reopened = Spool::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .append(ItemKind::Note, "01C", "c3", serde_json::json!({}))
            .unwrap(),
        Appended::Appended(3),
        "sequences continue from the journal"
    );
}

/// A torn final line (crash mid-append) is skipped on load, matching the worker's
/// journal discipline — infrastructure may lose a tail entry, never corrupt history.
#[test]
fn torn_trailing_lines_are_skipped_not_fatal() -> Result<(), SpoolError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("delegate-inbox");
    std::fs::create_dir_all(&path)?;
    let good = serde_json::to_string(&item(1, "c1")).map_err(|e| SpoolError::Json {
        context: "test item".into(),
        source: e,
    })?;
    let torn = r#"{"seq":2,"kind":"question","task_id":"01T","correlation_id":"c2""#;
    std::fs::write(path.join("items.jsonl"), format!("{good}\n{torn}\n"))
        .map_err(SpoolError::Io)?;

    let spool = Spool::open(dir.path())?;
    assert_eq!(spool.pending()?.len(), 1);
    Ok(())
}
