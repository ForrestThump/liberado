//! Split from `lib.rs` for module-health boundaries.

use super::*;

/// The source's wire name is how the daemon labels events it dispatches;
/// an empty or renamed source would mislabel every cron-fired goal.
#[test]
fn source_name_is_pinned_to_cron() {
    let src = CronEventSource::new(Vec::new()).expect("empty schedule set is valid");
    assert_eq!(EventSource::name(&src), "cron");
}
