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
//! own manifest. `CLAUDE.md` records it. Copying costs ~13 MB per worktree and cannot do that.
//! `.git` and `target` are skipped, which is most of the bulk and none of the value.

use std::path::Path;

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

/// Copy any declared path-dependency directory that `dest` is missing and `parent_root` has.
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

    let mut copied = Vec::new();
    for root in declared_path_dep_roots(&manifest) {
        let src = parent_root.join(&root);
        let dst = dest.join(&root);
        if dst.exists() {
            continue; // tracked in git, or already provisioned
        }
        if !src.is_dir() {
            // Declared but absent from the parent too. The parent cannot build either, so this
            // is the developer's setup problem, not something to paper over silently.
            tracing::warn!(
                dep = %root,
                "path dependency missing from the parent checkout; the worktree will not build"
            );
            continue;
        }
        match copy_tree(&src, &dst).await {
            Ok(()) => {
                tracing::info!(dep = %root, "provisioned path dependency into session worktree");
                copied.push(root);
            }
            Err(e) => tracing::warn!(dep = %root, error = %e, "copying path dependency failed"),
        }
    }
    copied
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
