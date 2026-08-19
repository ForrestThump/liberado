//! Provision the workspace's gitignored path dependencies into a session worktree.
//!
//! ## Why a coding worktree could not compile anything
//!
//! `turbovault/` and `turbomcp/` are path dependencies expected *inside* this repo and are
//! **gitignored**, so `git worktree add` never brings them along. Cargo then fails at manifest
//! resolution, before compiling a line:
//!
//! ```text
//! error: failed to load source for dependency `turbomcp`
//!   Unable to update .../coding-worktrees/lib-.../turbomcp/crates/turbomcp
//! ```
//!
//! Every coding run in a worktree therefore had **no way to build or test its own work**. That
//! is not a theoretical gap: an F6 run hit exactly this error, tried `cargo check` three times,
//! failed every time, and still filed `outcome: succeeded` with a mutation table describing
//! test failures it could not have observed. A model cannot verify what the harness will not
//! let it compile.
//!
//! ## The parent is not always where the files are
//!
//! The first version copied from the session worktree's immediate parent, which is right only
//! when that parent is the original checkout. Under Paseo it is not: Paseo gives each of its own
//! sessions a git worktree, and Liberado then creates the coding worktree inside *that*. A linked
//! worktree contains tracked files and nothing else, so the gitignored dependencies are absent
//! from it too — and provisioning dutifully copied nothing.
//!
//! The fix cost a run: the model noticed the missing dependency, cloned both repositories from
//! the network by hand, and only then could compile. It reached the right answer by luck — the
//! remotes had to be guessable and the network up — and a harness that needs luck is not fixed.
//!
//! So the sources are tried in order: the immediate parent first, then the repository's **main**
//! working tree, found via `git rev-parse --git-common-dir`. Nesting depth stops mattering.
//!
//! ## What this does
//!
//! Reads the parent's root manifest, finds every `path = "…"` dependency, takes the top-level
//! directory of each, and copies the ones missing from the worktree.
//!
//! **Discovered, not hardcoded.** Naming `turbovault` and `turbomcp` here would work today and
//! silently stop working the day a third is added — the same shape as the config values that
//! parsed and reached nobody. The manifest is the source of truth; this reads it.
//!
//! **Copied, never linked.** A junction is the obvious cheap alternative and it is a trap: in
//! this repo `git worktree remove --force` once followed `mklink /J` and deleted the *contents
//! of the originals*, leaving two empty directories and a workspace that could not resolve its
//! own manifest. `AGENTS.md` records it. Copying costs ~13 MB per worktree and cannot do that.
//! `.git` and `target` are skipped, which is most of the bulk and none of the value.

use std::path::{Path, PathBuf};

/// Top-level directories the root manifest expects as path dependencies.
///
/// Returns names like `turbovault`, deduped and sorted. Entries pointing outside the repo
/// (`../…`) are ignored: those live somewhere the worktree already shares.
pub fn declared_path_dep_roots(manifest: &str) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    let Ok(value) = manifest.parse::<toml::Value>() else {
        return roots;
    };

    let mut collect = |table: Option<&toml::Value>| {
        let Some(deps) = table.and_then(|t| t.as_table()) else {
            return;
        };
        for spec in deps.values() {
            let Some(path) = spec.get("path").and_then(|p| p.as_str()) else {
                continue;
            };
            // `turbovault/crates/turbovault-core` -> `turbovault`. Forward and back slashes,
            // because a manifest written on Windows is still a manifest.
            let first = path
                .split(['/', '\\'])
                .find(|s| !s.is_empty() && *s != ".")
                .unwrap_or_default();
            if first.is_empty() || first == ".." {
                continue;
            }
            if !roots.iter().any(|r| r == first) {
                roots.push(first.to_string());
            }
        }
    };

    collect(value.get("workspace").and_then(|w| w.get("dependencies")));
    collect(value.get("dependencies"));

    roots.sort();
    roots
}

/// The root of the **main** working tree of the repository containing `start`.
///
/// `git rev-parse --git-common-dir` names the `.git` directory shared by every worktree of a
/// repository. From a linked worktree that is the original checkout's, so its parent is the
/// directory where gitignored files actually live. From the main checkout it resolves to that
/// checkout, which makes this a no-op rather than a special case.
///
/// `None` when `start` is not in a git repository, or git is unavailable — both are conditions
/// the caller already tolerates, and neither is worth failing worktree creation over.
pub async fn main_worktree_root(start: &Path) -> Option<PathBuf> {
    let mut cmd = liberado_common::process::command("git");
    cmd.arg("-C")
        .arg(start)
        .args(["rev-parse", "--git-common-dir"]);
    let out = liberado_common::process::output_within(
        &mut cmd,
        "git rev-parse --git-common-dir",
        liberado_common::process::DEFAULT_COMMAND_TIMEOUT,
    )
    .await
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    // Relative inside the main checkout (`.git`), absolute from a linked worktree. Resolving the
    // relative case against `start` is what makes both shapes yield a real directory.
    let common = Path::new(&raw);
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        start.join(common)
    };
    let root = common.parent()?.to_path_buf();
    root.is_dir().then_some(root)
}

/// Copy any declared path-dependency directory the worktree is missing.
///
/// Sources are tried in order: `parent_root`, then the repository's main working tree. The second
/// exists because `parent_root` is itself a linked worktree whenever Liberado runs under Paseo,
/// and a linked worktree has no gitignored files to copy.
///
/// Best-effort and non-fatal: a worktree without them still *runs*, it just cannot compile, and
/// failing worktree creation outright would be a worse trade than a loud warning. Returns the
/// directories actually copied.
pub async fn provision_path_deps(parent_root: &Path, dest: &Path) -> Vec<String> {
    let manifest = match tokio::fs::read_to_string(parent_root.join("Cargo.toml")).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "cannot read root manifest; skipping path-dep provisioning");
            return Vec::new();
        }
    };

    let mut sources = vec![parent_root.to_path_buf()];
    if let Some(main) = main_worktree_root(parent_root).await
        && main != parent_root
    {
        sources.push(main);
    }

    let mut copied = Vec::new();
    for root in declared_path_dep_roots(&manifest) {
        if let Some(root) = provision_one_dep(&root, dest, &sources).await {
            copied.push(root);
        }
    }
    copied
}

/// Copy one declared path dependency into the worktree when a reachable source exists.
/// Returns the root name when a copy actually happened (skipped when already present).
async fn provision_one_dep(root: &str, dest: &Path, sources: &[PathBuf]) -> Option<String> {
    let dst = dest.join(root);
    if dst.exists() {
        return None; // tracked in git, or already provisioned
    }
    let Some(src) = sources.iter().map(|s| s.join(root)).find(|p| p.is_dir()) else {
        // Declared but present nowhere we can reach. The developer's own checkout cannot
        // build either, so this is a setup problem, not something to paper over silently.
        tracing::warn!(
            dep = %root,
            "path dependency is in neither the parent nor the main checkout; \
             the worktree will not build"
        );
        return None;
    };
    match copy_tree(&src, &dst).await {
        Ok(()) => {
            tracing::info!(dep = %root, "provisioned path dependency into session worktree");
            Some(root.to_string())
        }
        Err(e) => {
            tracing::warn!(dep = %root, error = %e, "copying path dependency failed");
            None
        }
    }
}

/// Directories never worth copying: `.git` is the dependency's own history and `target` is
/// build output that dwarfs the source and would be rebuilt anyway.
fn is_skipped(name: &str) -> bool {
    matches!(name, ".git" | "target")
}

/// Recursive copy on a blocking thread — `std::fs` walking is cheap and this is one-shot.
async fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let (src, dst) = (src.to_path_buf(), dst.to_path_buf());
    tokio::task::spawn_blocking(move || copy_tree_blocking(&src, &dst))
        .await
        .map_err(std::io::Error::other)?
}

fn copy_tree_blocking(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_skipped(&name.to_string_lossy()) {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        // `file_type` does not follow links, so a symlinked subtree is skipped rather than
        // silently duplicated or followed into a cycle.
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_tree_blocking(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
