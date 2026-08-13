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

    // `git worktree add` does not bring gitignored path-deps (`turbovault/`, `turbomcp/`).
    // Without them `cargo` dies at manifest resolution and the baseline is an opaque fail
    // that would hide real regressions if we treated it as "already broken". Copy, never
    // junction — `worktree remove --force` followed a junction once and emptied the originals.
    let _ = crate::provision_path_deps(opts.project_root, &worktree).await;

    // Warm build, and the agent's tree is untouched throughout. Restore whatever
    // `CARGO_TARGET_DIR` the caller had — compare runs share a host cache, and a
    // blanket `remove_var` used to wipe that for the rest of the process.
    let report = {
        let _target = opts.target_dir.map(CargoTargetDirGuard::set);
        run_preflight(&worktree, &subset).await
    };

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

/// Sets `CARGO_TARGET_DIR` for the baseline run and puts the previous value back on drop.
struct CargoTargetDirGuard {
    previous: Option<std::ffi::OsString>,
}

impl CargoTargetDirGuard {
    fn set(target: &Path) -> Self {
        let previous = std::env::var_os("CARGO_TARGET_DIR");
        // SAFETY: only held around `run_preflight` below; Drop restores.
        unsafe { std::env::set_var("CARGO_TARGET_DIR", target) };
        Self { previous }
    }
}

impl Drop for CargoTargetDirGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(v) => unsafe { std::env::set_var("CARGO_TARGET_DIR", v) },
            None => unsafe { std::env::remove_var("CARGO_TARGET_DIR") },
        }
    }
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

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A cheap failing step (no cargo) so we can prove compute records named identities
    /// without a workspace build.
    #[tokio::test]
    async fn compute_baseline_records_named_test_failures() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--quiet"]);
        git(root, &["config", "user.email", "test@liberado.local"]);
        git(root, &["config", "user.name", "liberado-test"]);
        std::fs::write(root.join("README"), "base\n").unwrap();
        git(root, &["add", "README"]);
        git(root, &["commit", "-q", "-m", "seed"]);
        let sha = String::from_utf8(
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(root)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // cmd.exe: echo the cargo-test FAILED line, then fail. Unix: same via sh -c.
        let run = if cfg!(windows) {
            "echo test initialize_and_session_new_over_stdio ... FAILED& exit /b 1".to_string()
        } else {
            "echo 'test initialize_and_session_new_over_stdio ... FAILED'; exit 1".to_string()
        };
        let spec = crate::PreflightSpec::new("ship", vec![crate::PreflightStep::new("test", run)]);
        let cache = root.join("cache");
        let opts = BaselineOptions {
            project_root: root,
            base_sha: &sha,
            cache_dir: &cache,
            target_dir: None,
        };
        let mut steps = BTreeSet::new();
        steps.insert("test".to_string());
        let set = compute_baseline(&opts, &spec, &steps)
            .await
            .expect("compute");
        let names: BTreeSet<_> = set.values().flatten().cloned().collect();
        assert!(
            names.contains("initialize_and_session_new_over_stdio"),
            "got {names:?}"
        );
        assert_eq!(
            load_baseline(&cache, &sha).as_ref(),
            Some(&set),
            "second call must be a cache hit"
        );
    }
}
