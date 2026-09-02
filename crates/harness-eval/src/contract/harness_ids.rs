//! Known comparison harness ids and the two legal adapter sets.

pub const HARNESS_LIBERADO: &str = "liberado";
pub const HARNESS_PI: &str = "pi";
pub const HARNESS_HERMES: &str = "hermes";
pub const HARNESS_DEEPAGENTS: &str = "deepagents";

/// The default run order: Liberado first, then pi. This is the historical order and the fallback
/// when a job does not declare one.
pub fn default_run_order() -> Vec<String> {
    vec![HARNESS_LIBERADO.to_string(), HARNESS_PI.to_string()]
}

/// Canonical four-harness C3 order. Rotation starts here so "who runs first" is fair.
pub fn default_four_way_run_order() -> Vec<String> {
    vec![
        HARNESS_LIBERADO.to_string(),
        HARNESS_PI.to_string(),
        HARNESS_HERMES.to_string(),
        HARNESS_DEEPAGENTS.to_string(),
    ]
}

pub fn is_known_harness_id(id: &str) -> bool {
    matches!(
        id,
        HARNESS_LIBERADO | HARNESS_PI | HARNESS_HERMES | HARNESS_DEEPAGENTS
    )
}

fn sorted_ids<'a>(ids: &'a [&str]) -> Vec<&'a str> {
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted
}

/// Liberado/Pi two-harness jobs. The historical v1 set.
pub fn is_two_way_set(ids: &[&str]) -> bool {
    sorted_ids(ids) == [HARNESS_LIBERADO, HARNESS_PI]
}

/// C3 four-harness set. Sorted comparison so request order does not matter.
pub fn is_four_way_set(ids: &[&str]) -> bool {
    sorted_ids(ids)
        == [
            HARNESS_DEEPAGENTS,
            HARNESS_HERMES,
            HARNESS_LIBERADO,
            HARNESS_PI,
        ]
}

/// The coordinator prepares worktrees for two-way Liberado/Pi jobs or the four-way C3 set.
pub fn is_supported_adapter_set(ids: &[&str]) -> bool {
    is_two_way_set(ids) || is_four_way_set(ids)
}

/// Alternate the run order for a new two-way job so the systematic "first harness" bias cancels
/// out across jobs. Even job counts run Liberado first; odd counts run pi first.
pub fn alternate_run_order(job_count: usize) -> Vec<String> {
    rotate_run_order(job_count, &default_run_order())
}

/// Rotate `harness_ids` so each job starts with a different harness. Two-way jobs keep today's
/// Liberado/pi alternation because that is `rotate_left` on [`default_run_order`].
pub fn rotate_run_order(job_count: usize, harness_ids: &[String]) -> Vec<String> {
    if harness_ids.is_empty() {
        return Vec::new();
    }
    let mut order = harness_ids.to_vec();
    let shift = job_count % order.len();
    order.rotate_left(shift);
    order
}

/// Canonical order used as the rotation base for a requested set.
pub fn canonical_run_order(ids: &[&str]) -> Vec<String> {
    if is_four_way_set(ids) {
        default_four_way_run_order()
    } else if is_two_way_set(ids) {
        default_run_order()
    } else {
        ids.iter().map(|id| (*id).to_string()).collect()
    }
}
