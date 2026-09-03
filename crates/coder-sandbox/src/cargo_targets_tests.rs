use super::*;
use std::time::Duration;

fn pool() -> (tempfile::TempDir, TargetPool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = TargetPool::new(dir.path().join("pool"));
    (dir, pool)
}

fn ordinary_request<'a>(root: &'a Path, job: Option<&'a str>) -> TargetRequest<'a> {
    TargetRequest {
        source_root: root,
        class: TargetClass::Ordinary,
        job_id: job,
        reclaim_on_drop: false,
    }
}

#[test]
fn ordinary_jobs_from_one_source_share_one_path() {
    let (dir, pool) = pool();
    let source = dir.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();

    let first = pool
        .allocate(&ordinary_request(&source, Some("a")))
        .unwrap();
    let second = pool
        .allocate(&ordinary_request(&source, Some("b")))
        .unwrap();

    assert_eq!(first.kind(), TargetKind::Shared);
    assert_eq!(second.path(), first.path());
    assert_eq!(
        std::fs::read_to_string(first.path().join(".liberado-target-class")).unwrap(),
        "ordinary"
    );
}

#[test]
fn distinct_source_roots_do_not_share() {
    let (dir, pool) = pool();
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();

    let first = pool.allocate(&ordinary_request(&one, Some("a"))).unwrap();
    let second = pool.allocate(&ordinary_request(&two, Some("b"))).unwrap();

    assert_ne!(first.path(), second.path());
}

#[test]
fn case_distinct_roots_keep_distinct_ordinary_caches_on_a_case_sensitive_filesystem() {
    let dir = tempfile::tempdir().expect("tempdir");
    let upper = dir.path().join("Repo");
    let lower = dir.path().join("repo");
    std::fs::create_dir(&upper).unwrap();
    let pool = TargetPool::new(dir.path().join("pool"));
    match std::fs::create_dir(&lower) {
        Ok(()) => {
            assert_ne!(
                pool.shared_path(&upper, TargetClass::Ordinary),
                pool.shared_path(&lower, TargetClass::Ordinary),
                "Linux-style distinct roots must not share a cache"
            );
        }
        Err(_) => {
            assert_eq!(
                pool.shared_path(&upper, TargetClass::Ordinary),
                pool.shared_path(&lower, TargetClass::Ordinary),
                "a case-insensitive filesystem treats the two spellings as one root"
            );
        }
    }
}

#[test]
fn case_sensitive_leaves_under_a_case_insensitive_ancestor_stay_distinct() {
    let ancestor_only = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("CiRoot"))
    };
    let upper = Path::new("/CiRoot/Repo");
    let lower = Path::new("/CiRoot/repo");
    let folded_upper = identity::fold_canonical_path(upper, ancestor_only);
    let folded_lower = identity::fold_canonical_path(lower, ancestor_only);
    assert_ne!(
        folded_upper, folded_lower,
        "a case-insensitive ancestor must not lowercase a case-sensitive leaf"
    );
    assert!(
        folded_upper.contains("Repo"),
        "the sensitive leaf must keep its case: {folded_upper}"
    );
    assert!(
        folded_lower.contains("repo"),
        "the sensitive leaf must keep its case: {folded_lower}"
    );
    assert!(
        folded_upper.to_ascii_lowercase().starts_with("/ciroot/"),
        "the insensitive ancestor still folds: {folded_upper}"
    );

    let fold_all = |_path: &Path| true;
    assert_eq!(
        identity::fold_canonical_path(upper, fold_all),
        identity::fold_canonical_path(lower, fold_all),
        "uniform case-insensitive trees still share one key"
    );
}

#[test]
fn a_worktree_does_not_inherit_another_project_root_ordinary_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    let worktree = dir.path().join("coding-worktrees").join("sess-1");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    let build = WorkspaceBuildConfig {
        managed_target_root: Some(dir.path().join("managed").to_string_lossy().into_owned()),
        ..WorkspaceBuildConfig::default()
    };
    let from_project = resolve_ordinary(&build, &project).unwrap();
    let from_worktree = resolve_ordinary(&build, &worktree).unwrap();
    assert_eq!(from_project.kind, TargetKind::Shared);
    assert_ne!(
        from_project.path, from_worktree.path,
        "managed identity is the job's source root, not a sibling worktree or process CWD"
    );
}

#[test]
fn coverage_mutation_and_comparison_never_use_the_ordinary_shared_path() {
    let (dir, pool) = pool();
    let source = dir.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();
    let shared = pool
        .allocate(&ordinary_request(&source, Some("a")))
        .unwrap();

    for class in [
        TargetClass::Coverage,
        TargetClass::Mutation,
        TargetClass::Comparison,
    ] {
        let lease = pool
            .allocate(&TargetRequest {
                source_root: &source,
                class,
                job_id: Some(class.slug()),
                reclaim_on_drop: false,
            })
            .unwrap();
        assert_eq!(lease.kind(), TargetKind::Isolated);
        assert_ne!(lease.path(), shared.path());
        assert!(
            lease.path().starts_with(pool.root().join("isolated")),
            "{}",
            lease.path().display()
        );
    }
}

#[test]
fn a_class_stamp_refuses_an_incompatible_reuse() {
    let (dir, pool) = pool();
    let source = dir.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();
    let shared = pool.shared_path(&source, TargetClass::Ordinary);
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join(".liberado-target-class"), "coverage").unwrap();

    let err = pool
        .allocate(&ordinary_request(&source, Some("a")))
        .expect_err("coverage stamp must block ordinary reuse");
    assert!(matches!(
        err,
        TargetError::Incompatible {
            existing: TargetClass::Coverage,
            requested: TargetClass::Ordinary,
            ..
        }
    ));
}

#[test]
fn isolated_lock_is_exclusive_while_the_holder_lives() {
    let (dir, pool) = pool();
    let source = dir.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();
    let request = TargetRequest {
        source_root: &source,
        class: TargetClass::Coverage,
        job_id: Some("cov-1"),
        reclaim_on_drop: false,
    };
    let held = pool.allocate(&request).unwrap();
    let err = pool
        .allocate(&request)
        .expect_err("live coverage job must keep its isolated target");
    assert!(matches!(err, TargetError::Busy { .. }));
    drop(held);
    assert!(pool.allocate(&request).is_ok());
}

#[test]
fn reclaim_on_drop_removes_only_the_isolated_target() {
    let (dir, pool) = pool();
    let source = dir.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();
    let shared = pool
        .allocate(&ordinary_request(&source, Some("a")))
        .unwrap();
    let isolated_path = {
        let lease = pool
            .allocate(&TargetRequest {
                source_root: &source,
                class: TargetClass::Mutation,
                job_id: Some("mut-1"),
                reclaim_on_drop: true,
            })
            .unwrap();
        lease.path().to_path_buf()
    };
    assert!(
        !isolated_path.exists(),
        "finished mutation target must be reclaimed"
    );
    assert!(shared.path().is_dir(), "shared ordinary cache must remain");
}

#[test]
fn reclaim_isolated_skips_a_live_lock_and_removes_a_stale_tree() {
    let (_dir, pool) = pool();
    let live = pool.isolated_path(TargetClass::Coverage, "live");
    let stale = pool.isolated_path(TargetClass::Coverage, "stale");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(live.join(".liberado-target-class"), "coverage").unwrap();
    std::fs::write(stale.join(".liberado-target-class"), "coverage").unwrap();
    std::fs::write(
        live.join(".liberado-target.lock"),
        format!("class=coverage\npid={}\n", std::process::id()),
    )
    .unwrap();
    std::fs::write(
        stale.join(".liberado-target.lock"),
        "class=coverage\npid=0\n",
    )
    .unwrap();

    let removed = pool.reclaim_isolated(Duration::from_secs(0)).unwrap();
    assert!(removed.iter().any(|p| p == &stale));
    assert!(live.is_dir(), "a live isolated job must not be reclaimed");
    assert!(!stale.exists());
}

#[test]
fn exact_shared_target_dir_is_used_as_the_ordinary_path() {
    let dir = tempfile::tempdir().unwrap();
    let exact = dir.path().join("c3-liberado");
    let build = WorkspaceBuildConfig {
        shared_target_dir: Some(exact.to_string_lossy().into_owned()),
        managed_target_root: Some(dir.path().join("managed").to_string_lossy().into_owned()),
        ..WorkspaceBuildConfig::default()
    };
    let allocation = resolve_ordinary(&build, dir.path()).unwrap();
    assert_eq!(allocation.path, exact);
    assert_eq!(allocation.kind, TargetKind::Shared);
}

#[test]
fn managed_root_without_an_exact_pin_uses_the_shared_ordinary_path() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("repo");
    std::fs::create_dir_all(&source).unwrap();
    let managed = dir.path().join("managed");
    let build = WorkspaceBuildConfig {
        managed_target_root: Some(managed.to_string_lossy().into_owned()),
        ..WorkspaceBuildConfig::default()
    };
    let allocation = resolve_ordinary(&build, &source).unwrap();
    assert_eq!(allocation.kind, TargetKind::Shared);
    assert!(allocation.path.starts_with(&managed));
    assert!(allocation.path.ends_with("ordinary"));
}

#[test]
fn unset_config_stays_worktree_local() {
    let dir = tempfile::tempdir().unwrap();
    let allocation = resolve_ordinary(&WorkspaceBuildConfig::default(), dir.path()).unwrap();
    assert_eq!(allocation.kind, TargetKind::WorktreeLocal);
    assert_eq!(allocation.path, dir.path().join("target"));
    assert!(
        ordinary_target_env(&WorkspaceBuildConfig::default(), dir.path())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn baseline_honors_a_live_cargo_target_dir() {
    let dir = tempfile::tempdir().unwrap();
    let override_dir = dir.path().join("from-env");
    let previous = std::env::var_os("CARGO_TARGET_DIR");
    unsafe { std::env::set_var("CARGO_TARGET_DIR", &override_dir) };
    let resolved = baseline_target_dir(None, dir.path());
    match previous {
        Some(value) => unsafe { std::env::set_var("CARGO_TARGET_DIR", value) },
        None => unsafe { std::env::remove_var("CARGO_TARGET_DIR") },
    }
    assert_eq!(resolved, override_dir);
}

#[test]
fn target_class_parse_rejects_unknown_names() {
    assert_eq!(TargetClass::parse("ordinary"), Some(TargetClass::Ordinary));
    assert_eq!(TargetClass::parse("nope"), None);
    assert!(!TargetClass::Coverage.may_share());
    assert!(TargetClass::Ordinary.may_share());
}
