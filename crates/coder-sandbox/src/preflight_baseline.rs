//! What was *already* failing at a commit, so preflight can gate on regressions.
//!
//! Preflight used to demand absolute green. That conflates two facts needing opposite responses
//! — "you broke it" must block, "it was already broken" must not — and it locks an agent out of
//! the work that *fixes* a red base. Worse than a wrong answer: the gate sits before terminal
//! `Succeeded`, so an agent meeting a pre-existing failure spends its whole attempt budget on
//! something it cannot fix.
//!
//! Two properties keep the cost sane:
//!
//! * **Lazy.** A baseline is only ever computed when preflight *fails*. A clean run costs
//!   exactly what it did before.
//! * **Cached per commit.** Every session sharing a base pays once, not once each.
//!
//! The baseline runs in a throwaway worktree at the base commit, so the agent's tree is never
//! touched — no stashing its work to peek underneath. `CARGO_TARGET_DIR` is pointed at the
//! caller's existing target directory, because a fresh worktree otherwise means a cold build and
//! the baseline would cost more than the work it guards.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::preflight::{
    FailureSet, OPAQUE_FAILURE, PreflightSpec, failure_identities, run_preflight,
};
use crate::{path_for_cli, run_git, run_git_best_effort};

/// Where a commit's baseline is cached.
pub fn baseline_cache_path(cache_dir: &Path, base_sha: &str) -> PathBuf {
    let short: String = base_sha.chars().take(12).collect();
    cache_dir.join(format!("{short}.json"))
}

/// Read a cached baseline, if one exists and parses.
///
/// A corrupt cache is treated as absent rather than fatal: the cost is recomputing, and the
/// alternative is a gate that stays broken until someone deletes a file by hand.
pub fn load_baseline(cache_dir: &Path, base_sha: &str) -> Option<FailureSet> {
    let raw = std::fs::read_to_string(baseline_cache_path(cache_dir, base_sha)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn store_baseline(cache_dir: &Path, base_sha: &str, failures: &FailureSet) {
    if std::fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_string_pretty(failures) {
        let _ = std::fs::write(baseline_cache_path(cache_dir, base_sha), json);
    }
}

/// Options for computing a baseline.
pub struct BaselineOptions<'a> {
    /// Repo the base commit lives in (the project checkout, not the session worktree).
    pub project_root: &'a Path,
    /// Commit the branch started from.
    pub base_sha: &'a str,
    /// Where baselines are cached, keyed by commit.
    pub cache_dir: &'a Path,
    /// Reused so the baseline build is warm. A cold build here can cost more than the work the
    /// gate is protecting.
    pub target_dir: Option<&'a Path>,
}

/// Failing identities at `base_sha`, for `steps_to_run` only.
///
/// Only the steps that already failed for the agent are re-run: if `test` failed and `clippy`
/// passed, nothing is learned by re-running `clippy` against the base, and it is the expensive
/// half of the profile.
pub async fn compute_baseline(
    opts: &BaselineOptions<'_>,
    spec: &PreflightSpec,
    steps_to_run: &BTreeSet<String>,
) -> Result<FailureSet, String> {
    if let Some(cached) = load_baseline(opts.cache_dir, opts.base_sha) {
        return Ok(cached);
    }

    let subset = PreflightSpec::new(
        spec.id.clone(),
        spec.steps
            .iter()
            .filter(|s| steps_to_run.contains(&s.name))
            .cloned()
            .collect(),
    );
    if subset.is_empty() {
        return Ok(FailureSet::new());
    }

    let short: String = opts.base_sha.chars().take(12).collect();
    let worktree = std::env::temp_dir().join(format!("liberado-baseline-{short}"));
    // A leftover from an interrupted run would be checked out at the right commit anyway, but
    // git refuses to re-add an existing path, so clear the registration first.
    let _ = std::fs::remove_dir_all(&worktree);
    run_git_best_effort(opts.project_root, &["worktree", "prune"]).await;

    let wt_cli = path_for_cli(&worktree);
    run_git(
        opts.project_root,
        &["worktree", "add", "--detach", &wt_cli, opts.base_sha],
    )
    .await
    .map_err(|e| format!("baseline worktree at {}: {e}", &short))?;

    // Warm build, and the agent's tree is untouched throughout.
    if let Some(target) = opts.target_dir {
        // SAFETY: process-wide, but preflight steps are spawned sequentially from here and the
        // variable is restored below before anything else runs.
        unsafe { std::env::set_var("CARGO_TARGET_DIR", target) };
    }
    let report = run_preflight(&worktree, &subset).await;
    if opts.target_dir.is_some() {
        unsafe { std::env::remove_var("CARGO_TARGET_DIR") };
    }

    // `remove` rather than `remove --force`, and never a recursive delete of the parent: a
    // force-remove follows directory links out of the worktree and can take real checkouts with
    // it. Leaving a stray temp dir is the cheaper failure.
    run_git_best_effort(opts.project_root, &["worktree", "remove", &wt_cli]).await;
    run_git_best_effort(opts.project_root, &["worktree", "prune"]).await;

    let report = report.map_err(|e| format!("baseline preflight: {e}"))?;
    let mut failures = FailureSet::new();
    for step in &report.steps {
        if step.ok {
            continue;
        }
        let mut ids = failure_identities(&step.log_excerpt);
        if ids.is_empty() {
            ids.insert(OPAQUE_FAILURE.to_string());
        }
        failures.insert(step.name.clone(), ids);
    }

    store_baseline(opts.cache_dir, opts.base_sha, &failures);
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_keyed_by_short_sha() {
        let p = baseline_cache_path(Path::new("/c"), "abcdef0123456789abcdef");
        assert!(p.ends_with("abcdef012345.json"), "{p:?}");
    }

    #[test]
    fn baseline_round_trips_through_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mut set = FailureSet::new();
        set.insert("test".into(), ["a::b".to_string()].into_iter().collect());

        store_baseline(dir.path(), "deadbeefcafe00", &set);
        assert_eq!(load_baseline(dir.path(), "deadbeefcafe00"), Some(set));
    }

    #[test]
    fn a_missing_baseline_reads_as_none_not_as_green() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_baseline(dir.path(), "0000000000"), None);
    }

    /// A corrupt cache must not wedge the gate — recomputing is cheap next to a permanently
    /// broken preflight.
    #[test]
    fn a_corrupt_cache_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(baseline_cache_path(dir.path(), "beefbeefbeef"), "{not json").unwrap();
        assert_eq!(load_baseline(dir.path(), "beefbeefbeef"), None);
    }
}
