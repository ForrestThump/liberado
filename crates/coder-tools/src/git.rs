//! Library-backed git operations (backlog item C1).
//!
//! The coding pack's dedicated git tools run through gix (gitoxide) where the API covers the
//! operation, and through a strictly validated per-tool argv otherwise. `CommandPolicy::default()`
//! denies `git` (and `git.exe`) so `run_command` is closed; this module is the sanctioned path
//! the capability model can see.

use std::path::Path;

use gix::bstr::{BStr, ByteSlice};

/// Error carrying a git-like exit code (128 for read, 1 for write) and a message.
#[derive(Debug)]
pub struct GitError {
    pub exit_code: i32,
    pub message: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for GitError {}

fn read_err(e: impl std::fmt::Display) -> GitError {
    GitError {
        exit_code: 128,
        message: e.to_string(),
    }
}

fn write_err(e: impl std::fmt::Display) -> GitError {
    GitError {
        exit_code: 1,
        message: e.to_string(),
    }
}

fn open_repo(root: &Path) -> Result<gix::Repository, GitError> {
    open_repo_with(root, gix::open::Options::default())
}

fn open_repo_with(root: &Path, options: gix::open::Options) -> Result<gix::Repository, GitError> {
    let mut repo = gix::open_opts(root, options).map_err(|e| GitError {
        exit_code: 128,
        message: format!("fatal: not a git repository: {e}"),
    })?;
    ensure_agent_identity(&mut repo)?;
    Ok(repo)
}

/// gix refuses to write a reflog without a committer. Dev machines have
/// `user.name` in the global config; CI runners do not. Install the same
/// agent identity [`commit`] already uses, but only when nothing else is set.
fn ensure_agent_identity(repo: &mut gix::Repository) -> Result<(), GitError> {
    if repo.committer().is_some() {
        return Ok(());
    }
    {
        let mut snap = repo.config_snapshot_mut();
        snap.set_value(
            &gix::config::tree::User::NAME,
            gix::bstr::BStr::new(b"liberado"),
        )
        .map_err(write_err)?;
        snap.set_value(
            &gix::config::tree::User::EMAIL,
            gix::bstr::BStr::new(b"liberado@local"),
        )
        .map_err(write_err)?;
        snap.commit().map_err(write_err)?;
    }
    if repo.committer().is_none() {
        return Err(GitError {
            exit_code: 1,
            message: "could not install agent committer identity".to_string(),
        });
    }
    Ok(())
}

/// The signature used for agent-authored commits — the identity the shell tools previously
/// supplied via `GIT_AUTHOR_NAME` / `GIT_COMMITTER_NAME`.
fn agent_sig_inner(time_str: &str) -> gix::actor::SignatureRef<'_> {
    gix::actor::SignatureRef {
        name: gix::bstr::BStr::new(b"liberado"),
        email: gix::bstr::BStr::new(b"liberado@local"),
        time: time_str,
    }
}

/// A `BStr` repo path rendered for display.
fn bs_display(b: &BStr) -> String {
    b.to_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|_| format!("{b:?}"))
}

/// Execute `git` with a fixed, tool-validated argv (bypasses the sandbox command policy,
/// which denies git for `run_command`). The argv is constructed in this module only, from
/// inputs validated by the calling tool; it is not a shell.
fn run_git_tool(root: &Path, args: &[&str]) -> Result<String, GitError> {
    let out = liberado_common::process::std_command("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "liberado")
        .env("GIT_AUTHOR_EMAIL", "liberado@local")
        .env("GIT_COMMITTER_NAME", "liberado")
        .env("GIT_COMMITTER_EMAIL", "liberado@local")
        .output()
        .map_err(|e| GitError {
            exit_code: 1,
            message: format!("failed to spawn git: {e}"),
        })?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        return Err(GitError {
            exit_code: out.status.code().unwrap_or(1),
            message: stderr.trim().to_string(),
        });
    }
    Ok(stdout)
}

// ---------------------------------------------------------------------------
// Public API — one function per coder tool
// ---------------------------------------------------------------------------

/// `git status --porcelain`.
pub fn status(root: &Path) -> Result<String, GitError> {
    let repo = open_repo(root)?;
    let platform = repo.status(gix::progress::Discard).map_err(read_err)?;
    let iter = platform.into_iter([]).map_err(read_err)?;

    let mut lines = Vec::new();
    for item in iter {
        let item = item.map_err(read_err)?;
        let loc = bs_display(item.location());
        match &item {
            gix::status::Item::IndexWorktree(change) => {
                // Stat-only updates are not user-visible.
                let Some(code) = index_worktree_code(change.summary()) else {
                    continue;
                };
                lines.push(format!("{code} {loc}"));
            }
            gix::status::Item::TreeIndex(change) => {
                let code = tree_index_code(change);
                lines.push(format!("{code} {loc}"));
            }
        }
    }
    Ok(lines.join("\n"))
}

/// The porcelain code for one IndexWorktree change; `None` for a stat-only update.
fn index_worktree_code(
    summary: Option<gix::status::index_worktree::iter::Summary>,
) -> Option<&'static str> {
    use gix::status::index_worktree::iter::Summary as S;
    match summary {
        Some(S::Added) => Some("??"),
        Some(S::Modified) => Some(" M"),
        Some(S::Removed) => Some(" D"),
        Some(S::TypeChange) => Some(" T"),
        Some(S::Renamed) => Some(" R"),
        Some(S::Copied) => Some(" C"),
        Some(S::IntentToAdd) => Some(" A"),
        Some(S::Conflict) => Some("UU"),
        None => None,
    }
}

/// The porcelain code for one TreeIndex change.
fn tree_index_code(change: &gix::diff::index::ChangeRef) -> &'static str {
    match change {
        gix::diff::index::ChangeRef::Addition { .. } => "A ",
        gix::diff::index::ChangeRef::Deletion { .. } => "D ",
        gix::diff::index::ChangeRef::Modification { .. } => "M ",
        gix::diff::index::ChangeRef::Rewrite { .. } => "R ",
    }
}

/// Untracked files (`git ls-files --others --exclude-standard`).
pub fn untracked_files(root: &Path) -> Result<Vec<String>, GitError> {
    let repo = open_repo(root)?;
    let platform = repo.status(gix::progress::Discard).map_err(read_err)?;
    let iter = platform.into_iter([]).map_err(read_err)?;

    let mut out = Vec::new();
    for item in iter {
        let Ok(item) = item else { continue };
        if let gix::status::Item::IndexWorktree(
            gix::status::index_worktree::Item::DirectoryContents { entry, .. },
        ) = item
        {
            out.push(bs_display(entry.rela_path.as_ref()));
        }
    }
    Ok(out)
}

/// `git checkout -b <name>` — create a branch at HEAD and switch to it.
pub fn branch_create(root: &Path, name: &str) -> Result<(), GitError> {
    let repo = open_repo(root)?;
    branch_create_in(&repo, name)
}

fn branch_create_in(repo: &gix::Repository, name: &str) -> Result<(), GitError> {
    let head_id = repo.head_id().map_err(write_err)?;

    let full_name: gix::refs::FullName = format!("refs/heads/{name}").try_into().map_err(
        |e: gix::validate::reference::name::Error| GitError {
            exit_code: 1,
            message: format!("invalid branch name '{name}': {e}"),
        },
    )?;

    // `git checkout -b` fails when the branch already exists.
    repo.reference(
        full_name.clone(),
        head_id,
        gix::refs::transaction::PreviousValue::MustNotExist,
        "",
    )
    .map_err(write_err)?;

    let head_ref: gix::refs::FullName = "HEAD".try_into().expect("HEAD is valid");
    repo.edit_reference(gix::refs::transaction::RefEdit {
        change: gix::refs::transaction::Change::Update {
            log: gix::refs::transaction::LogChange {
                mode: gix::refs::transaction::RefLog::AndReference,
                force_create_reflog: false,
                message: format!("checkout: moving from HEAD to {name}").into(),
            },
            expected: gix::refs::transaction::PreviousValue::Any,
            new: gix::refs::Target::Symbolic(full_name),
        },
        name: head_ref,
        deref: false,
    })
    .map_err(write_err)?;

    Ok(())
}

/// `git add` + `git commit -m <message>`. Stages everything when `files` is empty/None.
pub fn commit(root: &Path, message: &str, files: Option<&[String]>) -> Result<String, GitError> {
    let repo = open_repo(root)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let time_str = format!("{now} +0000");
    let sig = agent_sig_inner(&time_str);

    // Stage. `git add -A` / `git add -- <path>` are fixed argv shapes; the only variable
    // input is the path list, each validated below.
    match files {
        Some(list) if !list.is_empty() => {
            for f in list {
                if f.is_empty() || f.starts_with('-') {
                    return Err(GitError {
                        exit_code: 1,
                        message: format!("invalid file path: '{f}'"),
                    });
                }
                run_git_tool(root, &["add", "--", f])?;
            }
        }
        _ => {
            run_git_tool(root, &["add", "-A"])?;
        }
    }

    // gix-index 0.54 has no index-to-tree writer, so `git write-tree` (fixed argv, no
    // variable input) supplies the tree id for the new commit.
    let tree_hex = run_git_tool(root, &["write-tree"])?;
    let tree_id =
        gix::hash::ObjectId::from_hex(tree_hex.trim().as_bytes()).map_err(|e| GitError {
            exit_code: 1,
            message: format!("bad tree hash: {e}"),
        })?;

    let parents: Vec<gix::hash::ObjectId> = match repo.head_id() {
        Ok(id) => vec![id.into()],
        Err(_) => Vec::new(),
    };

    // `git commit` (without --allow-empty) refuses a commit that changes nothing. The staged
    // tree equals the HEAD tree exactly when there is nothing to commit.
    if let Ok(head_tree) = repo.head_tree_id() {
        let head_tree: gix::hash::ObjectId = head_tree.into();
        if head_tree == tree_id {
            return Err(GitError {
                exit_code: 1,
                message: "nothing to commit, working tree clean".to_string(),
            });
        }
    }

    let commit_id = repo
        .commit_as(sig, sig, "HEAD", message, tree_id, parents)
        .map_err(write_err)?;

    Ok(format!("[{commit_id:.7}] {message}"))
}

/// `git push [--set-upstream] <remote> [<branch>]`.
pub fn push(
    root: &Path,
    remote: &str,
    branch: Option<&str>,
    set_upstream: bool,
) -> Result<String, GitError> {
    let mut args = vec!["push".to_string()];
    if set_upstream {
        args.push("--set-upstream".to_string());
    }
    args.push(remote.to_string());
    if let Some(b) = branch {
        args.push(b.to_string());
    }
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git_tool(root, &argv)
}

/// `git fetch <remote> [<branch>]`.
pub fn fetch(root: &Path, remote: &str, branch: Option<&str>) -> Result<String, GitError> {
    match branch {
        Some(b) => run_git_tool(root, &["fetch", remote, b]),
        None => run_git_tool(root, &["fetch", remote]),
    }
}

/// `git merge [--ff-only] <branch>`.
pub fn merge(root: &Path, branch: &str, fast_forward_only: bool) -> Result<String, GitError> {
    let mut args = vec!["merge".to_string()];
    if fast_forward_only {
        args.push("--ff-only".to_string());
    }
    args.push(branch.to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git_tool(root, &argv)
}

/// `git log --max-count=<limit> [<branch>]`, formatted like `%h %s` by default.
///
/// A custom `format` (git's `--format` spec) is passed through to git as a single argv
/// entry; the gix walk covers only the common shorthand.
pub fn log(
    root: &Path,
    limit: u32,
    format: Option<&str>,
    branch: Option<&str>,
) -> Result<String, GitError> {
    match format {
        Some(fmt) if !fmt.is_empty() && fmt != "%h %s" => {
            let mut args = vec![
                "log".to_string(),
                format!("--max-count={limit}"),
                format!("--format={fmt}"),
            ];
            if let Some(b) = branch {
                args.push(b.to_string());
            }
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            return run_git_tool(root, &argv);
        }
        _ => {}
    }
    let repo = open_repo(root)?;

    let tip: gix::hash::ObjectId = match branch {
        Some(b) => {
            let mut reference = repo
                .find_reference(&format!("refs/heads/{b}"))
                .map_err(|e| GitError {
                    exit_code: 128,
                    message: format!("fatal: '{b}': {e}"),
                })?;
            reference
                .peel_to_id()
                .map_err(|e| GitError {
                    exit_code: 128,
                    message: format!("fatal: '{b}': {e}"),
                })?
                .into()
        }
        None => repo.head_id().map_err(read_err)?.into(),
    };

    let mut out = String::new();
    let mut walk = repo.rev_walk([tip]).all().map_err(read_err)?;
    for _ in 0..limit {
        let Some(info) = walk.next() else { break };
        let info = info.map_err(read_err)?;
        let commit = repo.find_commit(info.id()).map_err(read_err)?;
        let subject = commit
            .message()
            .ok()
            .and_then(|m| m.title.to_str().ok())
            .unwrap_or("")
            .to_string();
        let short = info
            .id()
            .shorten()
            .map(|p| p.to_string())
            .unwrap_or_else(|_| format!("{}", info.id()));
        out.push_str(&format!("{short} {subject}\n"));
    }
    Ok(out)
}

/// `git diff --name-only` (tracked changes).
pub fn diff_name_only(root: &Path) -> Result<String, GitError> {
    run_git_tool(root, &["diff", "--name-only"])
}

/// `git diff --stat` (tracked changes).
pub fn diff_stat(root: &Path) -> Result<String, GitError> {
    run_git_tool(root, &["diff", "--stat"])
}

/// `git diff` (patch mode, tracked changes).
pub fn diff_patch(root: &Path) -> Result<String, GitError> {
    run_git_tool(root, &["diff"])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seed a repo with env identity only — no `user.name` in the repo config.
    /// Isolated open must then see no committer; that is the CI runner's world.
    fn repo_without_user_config() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@test")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@test")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "--quiet"]);
        std::fs::write(dir.path().join("seed.txt"), "initial\n").unwrap();
        run(&["add", "seed.txt"]);
        run(&["commit", "-m", "initial"]);
        dir
    }

    #[test]
    fn isolated_open_has_no_committer_without_fallbacks() {
        let dir = repo_without_user_config();
        let repo = gix::open_opts(dir.path(), gix::open::Options::isolated()).unwrap();
        assert!(
            repo.committer().is_none(),
            "fixture must not carry a committer or the identity fallback is untested"
        );
    }

    #[test]
    fn branch_create_works_without_host_identity() {
        let dir = repo_without_user_config();
        let repo = open_repo_with(dir.path(), gix::open::Options::isolated()).unwrap();
        assert!(
            repo.committer().is_some(),
            "open_repo_with must install the agent identity when the host has none"
        );
        branch_create_in(&repo, "feature-x").expect("branch create without host identity");
    }
}
