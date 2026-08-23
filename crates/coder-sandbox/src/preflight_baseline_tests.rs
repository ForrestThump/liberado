//! Split from `preflight_baseline.rs` for module-health boundaries.

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
    let out = liberado_common::process::std_command("git")
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
        liberado_common::process::std_command("git")
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
