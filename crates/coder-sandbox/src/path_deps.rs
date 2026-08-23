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
#[path = "path_deps_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "path_deps_survivor_tests.rs"]
mod survivor_tests;
