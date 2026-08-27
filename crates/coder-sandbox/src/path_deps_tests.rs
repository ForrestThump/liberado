//! Split from `path_deps.rs` for module-health boundaries.

use super::*;

#[test]
fn roots_are_discovered_from_the_manifest_not_hardcoded() {
    let manifest = r#"
[workspace.dependencies]
turbovault-core = { path = "turbovault/crates/turbovault-core" }
turbomcp = { path = "turbomcp/crates/turbomcp" }
serde = "1"
external = { path = "../outside/thing" }
"#;
    let roots = declared_path_dep_roots(manifest);
    assert_eq!(
        roots,
        vec!["turbomcp".to_string(), "turbovault".to_string()],
        "must dedupe to top-level dirs and ignore non-path and out-of-repo deps"
    );
}

/// The real manifest is the case that matters — a parser that handles the fixture but not
/// the file it exists for is worth nothing.
#[test]
fn the_real_root_manifest_declares_the_expected_roots() {
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("Cargo.toml"),
    )
    .expect("read root Cargo.toml");
    let roots = declared_path_dep_roots(&manifest);
    assert!(
        roots.contains(&"turbovault".to_string()) && roots.contains(&"turbomcp".to_string()),
        "the sibling checkouts CI clones must be discovered here, got {roots:?}"
    );
}

#[tokio::test]
async fn a_missing_dependency_is_copied_and_build_output_is_left_behind() {
    let parent = tempfile::tempdir().expect("parent");
    let dest = tempfile::tempdir().expect("dest");

    std::fs::write(
        parent.path().join("Cargo.toml"),
        "[workspace.dependencies]\ndep = { path = \"vendored/crates/dep\" }\n",
    )
    .expect("manifest");
    let src = parent.path().join("vendored/crates/dep");
    std::fs::create_dir_all(&src).expect("src");
    std::fs::write(src.join("lib.rs"), "// source").expect("lib.rs");
    std::fs::create_dir_all(parent.path().join("vendored/target/debug")).expect("target");
    std::fs::write(
        parent.path().join("vendored/target/debug/huge.bin"),
        "build output",
    )
    .expect("artifact");
    std::fs::create_dir_all(parent.path().join("vendored/.git")).expect(".git");

    let copied = provision_path_deps(parent.path(), dest.path()).await;

    assert_eq!(copied, vec!["vendored".to_string()]);
    assert!(
        dest.path().join("vendored/crates/dep/lib.rs").is_file(),
        "source must be copied, or the worktree still cannot resolve the manifest"
    );
    assert!(
        !dest.path().join("vendored/target").exists(),
        "build output must be skipped"
    );
    assert!(
        !dest.path().join("vendored/.git").exists(),
        "the dependency's own history must be skipped"
    );
}

/// A git repo with one commit, plus a linked worktree of it. Identity is passed with `-c`:
/// `user.name`/`user.email` exist on every dev machine and on no CI runner.
fn repo_with_linked_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let main = dir.path().join("checkout");
    std::fs::create_dir_all(&main).expect("checkout");
    let m = main.to_string_lossy().to_string();
    let git = |args: &[&str]| {
        let out = liberado_common::process::std_command("git")
            .args(args)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["-C", &m, "init", "-q"]);
    std::fs::write(main.join("tracked.txt"), "tracked\n").expect("tracked");
    git(&["-C", &m, "add", "-A"]);
    git(&[
        "-C",
        &m,
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "seed",
    ]);
    let linked = dir.path().join("linked");
    git(&[
        "-C",
        &m,
        "worktree",
        "add",
        "-q",
        &linked.to_string_lossy(),
        "-b",
        "linked",
    ]);
    (dir, main, linked)
}

/// From a linked worktree, the main checkout must be found — that is where gitignored files
/// are. From the main checkout the answer is itself, so callers need no special case.
#[tokio::test]
async fn the_main_checkout_is_found_from_a_linked_worktree() {
    let (_guard, main, linked) = repo_with_linked_worktree();

    let from_linked = main_worktree_root(&linked)
        .await
        .expect("a linked worktree must resolve to its main checkout");
    assert_eq!(
        std::fs::canonicalize(&from_linked).expect("canon"),
        std::fs::canonicalize(&main).expect("canon"),
        "resolved {from_linked:?}, which is not the checkout holding the ignored files"
    );

    let from_main = main_worktree_root(&main)
        .await
        .expect("the main checkout must resolve to itself");
    assert_eq!(
        std::fs::canonicalize(&from_main).expect("canon"),
        std::fs::canonicalize(&main).expect("canon"),
    );
}

#[tokio::test]
async fn a_non_repository_has_no_main_checkout() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(
        main_worktree_root(dir.path()).await.is_none(),
        "outside a repository there is nothing to resolve, and guessing would be worse"
    );
}

/// The failure this function was rewritten for: Liberado under Paseo builds its coding
/// worktree inside a Paseo worktree, and a linked worktree carries no gitignored files. The
/// first version copied from that parent, found nothing, and left a workspace that could not
/// compile — so the model cloned the dependency off the network by hand to make progress.
#[tokio::test]
async fn a_dependency_missing_from_a_linked_parent_is_taken_from_the_main_checkout() {
    let (_guard, main, linked) = repo_with_linked_worktree();
    let dest = tempfile::tempdir().expect("dest");

    // Gitignored, so it exists only in the main checkout — never in the linked worktree.
    let vendored = main.join("vendored/crates/dep");
    std::fs::create_dir_all(&vendored).expect("vendored");
    std::fs::write(vendored.join("lib.rs"), "// source").expect("lib.rs");
    std::fs::write(
        linked.join("Cargo.toml"),
        "[workspace.dependencies]\ndep = { path = \"vendored/crates/dep\" }\n",
    )
    .expect("manifest");
    assert!(
        !linked.join("vendored").exists(),
        "precondition: the linked worktree must not have it"
    );

    let copied = provision_path_deps(&linked, dest.path()).await;

    assert_eq!(
        copied,
        vec!["vendored".to_string()],
        "the dependency must be found one level up, not reported as missing"
    );
    assert!(
        dest.path().join("vendored/crates/dep/lib.rs").is_file(),
        "without this the worktree cannot resolve its own manifest"
    );
}

/// Headless compare trees are HostLocal on a linked worktree: dest is the worktree itself.
#[tokio::test]
async fn a_linked_worktree_used_as_its_own_dest_gets_the_main_checkouts_deps() {
    let (_guard, main, linked) = repo_with_linked_worktree();
    std::fs::write(
        linked.join("Cargo.toml"),
        "[workspace.dependencies]\ndep = { path = \"vendored/crates/dep\" }\n",
    )
    .expect("manifest");
    let vendored = main.join("vendored/crates/dep");
    std::fs::create_dir_all(&vendored).expect("vendored");
    std::fs::write(vendored.join("lib.rs"), "// source").expect("lib.rs");

    let copied = provision_path_deps(&linked, &linked).await;

    assert_eq!(copied, vec!["vendored".to_string()]);
    assert!(
        linked.join("vendored/crates/dep/lib.rs").is_file(),
        "HostLocal on a git worktree must resolve cargo path-deps"
    );
}

#[tokio::test]
async fn an_already_present_directory_is_not_overwritten() {
    let parent = tempfile::tempdir().expect("parent");
    let dest = tempfile::tempdir().expect("dest");
    std::fs::write(
        parent.path().join("Cargo.toml"),
        "[workspace.dependencies]\ndep = { path = \"vendored/crates/dep\" }\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(parent.path().join("vendored")).expect("src");
    std::fs::write(parent.path().join("vendored/from-parent.txt"), "parent").expect("f");

    // The worktree already has it (e.g. someone started tracking it in git).
    std::fs::create_dir_all(dest.path().join("vendored")).expect("dest dep");
    std::fs::write(dest.path().join("vendored/local.txt"), "local").expect("f");

    let copied = provision_path_deps(parent.path(), dest.path()).await;

    assert!(copied.is_empty(), "must not re-provision what is present");
    assert!(
        !dest.path().join("vendored/from-parent.txt").exists(),
        "an existing directory must be left exactly as it was"
    );
}
