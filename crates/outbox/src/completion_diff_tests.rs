use std::collections::BTreeMap;

use super::{SnapshotChange, TaskCounts, diff_snapshot};

fn counts(open: i64, closed: i64) -> TaskCounts {
    TaskCounts { open, closed }
}

fn map(pairs: &[(&str, TaskCounts)]) -> BTreeMap<String, TaskCounts> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), *value))
        .collect()
}

fn completed(changes: &[SnapshotChange], text: &str) -> Option<u32> {
    changes.iter().find_map(|change| match change {
        SnapshotChange::Completed {
            task_text, count, ..
        } if task_text == text => Some(*count),
        _ => None,
    })
}

#[test]
fn a_plain_checkoff_is_one_completion() {
    let prev = map(&[("call", counts(1, 0))]);
    let now = map(&[("call", counts(0, 1))]);
    let changes = diff_snapshot(&prev, &now);
    assert_eq!(completed(&changes, "call"), Some(1));
}

#[test]
fn a_rollover_keeps_one_open_line_and_logs_one_row() {
    let prev = map(&[("call", counts(1, 0))]);
    let now = map(&[("call", counts(1, 1))]);
    assert_eq!(completed(&diff_snapshot(&prev, &now), "call"), Some(1));
}

#[test]
fn a_checked_line_with_no_prior_open_line_logs_nothing() {
    let prev = map(&[("call", counts(0, 0))]);
    let now = map(&[("call", counts(0, 1))]);
    assert!(diff_snapshot(&prev, &now).is_empty());
    assert!(diff_snapshot(&BTreeMap::new(), &now).is_empty());
}

#[test]
fn an_uncheck_voids_one_and_a_deleted_done_line_does_not() {
    let checked = map(&[("call", counts(0, 1))]);
    let open = map(&[("call", counts(1, 0))]);
    let changes = diff_snapshot(&checked, &open);
    assert_eq!(
        changes,
        vec![SnapshotChange::Voided {
            task_text: "call".into(),
            count: 1,
        }]
    );
    assert!(diff_snapshot(&checked, &BTreeMap::new()).is_empty());
}

#[test]
fn two_open_lines_and_one_check_log_one_row() {
    let prev = map(&[("call", counts(2, 0))]);
    let now = map(&[("call", counts(1, 1))]);
    assert_eq!(completed(&diff_snapshot(&prev, &now), "call"), Some(1));
}

#[test]
fn an_unchanged_snapshot_and_a_new_open_line_log_nothing() {
    let same = map(&[("call", counts(1, 0))]);
    assert!(diff_snapshot(&same, &same).is_empty());
    let added = map(&[("call", counts(1, 0)), ("other", counts(1, 0))]);
    assert!(diff_snapshot(&same, &added).is_empty());
}

#[test]
fn a_reworded_checkoff_is_a_new_identity_with_no_row() {
    let prev = map(&[("alpha", counts(1, 0))]);
    let now = map(&[("beta", counts(0, 1))]);
    assert!(diff_snapshot(&prev, &now).is_empty());
}

#[test]
fn only_the_identity_that_closed_is_logged() {
    let prev = map(&[("a", counts(1, 0)), ("b", counts(1, 0))]);
    let now = map(&[("a", counts(0, 1)), ("b", counts(1, 0))]);
    let changes = diff_snapshot(&prev, &now);
    assert_eq!(changes.len(), 1);
    assert_eq!(completed(&changes, "a"), Some(1));
}
