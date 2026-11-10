//! Count diff for one note's task identities.
//!
//! The tick compares the previous snapshot with the tasks it sees now. It does
//! not match lines. A missing key is `(0, 0)`, so the first sighting of a file
//! only seeds the snapshot.

use std::collections::{BTreeMap, BTreeSet};

/// Open and closed counts for one identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskCounts {
    pub open: i64,
    pub closed: i64,
}

/// One identity's count change. `count` is how many log rows to insert or void.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotChange {
    Completed {
        task_text: String,
        count: u32,
        prior_closed: i64,
    },
    Voided {
        task_text: String,
        count: u32,
    },
}

/// Compare two snapshots of one note. Keys are task identities.
pub fn diff_snapshot(
    prev: &BTreeMap<String, TaskCounts>,
    now: &BTreeMap<String, TaskCounts>,
) -> Vec<SnapshotChange> {
    let mut keys = BTreeSet::new();
    keys.extend(prev.keys().cloned());
    keys.extend(now.keys().cloned());
    let mut changes = Vec::new();
    for key in keys {
        let before = prev.get(&key).copied().unwrap_or_default();
        let after = now.get(&key).copied().unwrap_or_default();
        if let Some(change) = change_for(&key, before, after) {
            changes.push(change);
        }
    }
    changes
}

fn change_for(task_text: &str, prev: TaskCounts, now: TaskCounts) -> Option<SnapshotChange> {
    let closed_delta = now.closed.saturating_sub(prev.closed);
    if closed_delta > 0 && prev.open >= 1 {
        return Some(SnapshotChange::Completed {
            task_text: task_text.to_string(),
            count: as_count(closed_delta),
            prior_closed: prev.closed,
        });
    }
    let open_delta = now.open.saturating_sub(prev.open);
    if closed_delta < 0 && open_delta > 0 {
        let voids = closed_delta.saturating_neg().min(open_delta);
        return Some(SnapshotChange::Voided {
            task_text: task_text.to_string(),
            count: as_count(voids),
        });
    }
    None
}

fn as_count(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[path = "completion_diff_tests.rs"]
mod completion_diff_tests;
