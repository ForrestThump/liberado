//! Git plumbing for linked worktree setup.

use std::path::Path;
use std::time::Duration;

use liberado_coder_core::CommandPolicy;

use crate::{SandboxError, WorktreeWorkspace, path_for_cli};

/// Ceiling for the local git plumbing that sets up a worktree.
const GIT_TIMEOUT: Duration = liberado_common::process::DEFAULT_COMMAND_TIMEOUT;

pub(super) async fn create_linked_worktree(
    parent_root: &Path,
    dest: &Path,
    revision: Option<&str>,
) -> Result<(), SandboxError> {
    let parent_cli = path_for_cli(parent_root);
    let dest_cli = path_for_cli(dest);
    let _registry = crate::worktree_registry::lock().await;
    #[cfg(test)]
    let _depth = crate::worktree_registry::enter_probe();

    let mut prune = liberado_common::process::command("git");
    prune.args(["-C", &parent_cli]).args(["worktree", "prune"]);
    if let Err(e) =
        liberado_common::process::output_within(&mut prune, "git worktree prune", GIT_TIMEOUT).await
    {
        tracing::warn!(%e, "git worktree prune did not complete; continuing to worktree add");
    }

    let mut add = liberado_common::process::command("git");
    add.args(["-C", &parent_cli]).args(["worktree", "add"]);
    if revision.is_some() {
        add.arg("--detach");
    }
    add.args(["--no-checkout", &dest_cli]);
    if let Some(revision) = revision {
        add.arg(revision);
    }
    let output = liberado_common::process::output_within(&mut add, "git worktree add", GIT_TIMEOUT)
        .await
        .map_err(|e| SandboxError::Spawn(format!("git worktree add: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SandboxError::Spawn(format!(
            "git worktree add failed: {stderr}"
        )));
    }

    let mut checkout = liberado_common::process::command("git");
    checkout
        .args(["-C", &dest_cli])
        .args(["checkout", revision.unwrap_or("HEAD"), "--"]);
    let output =
        liberado_common::process::output_within(&mut checkout, "git checkout", GIT_TIMEOUT)
            .await
            .map_err(|e| SandboxError::Spawn(format!("git checkout in worktree: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(dest);
        return Err(SandboxError::Spawn(format!(
            "git checkout in worktree failed: {stderr}"
        )));
    }

    crate::path_deps::provision_path_deps(parent_root, dest).await;
    Ok(())
}

impl WorktreeWorkspace {
    /// Create a fresh detached worktree at an exact revision.
    ///
    /// This is the review counterpart to [`Self::new`]: the caller binds the observed PR head,
    /// while the review process receives a read-only command policy of its own.
    pub async fn new_at(
        parent_root: &Path,
        revision: &str,
        session_id: &str,
        worktrees_base: &Path,
        command_policy: CommandPolicy,
    ) -> Result<Self, SandboxError> {
        if revision.trim().is_empty() {
            return Err(SandboxError::MissingRoot(
                "pinned worktree revision is empty".into(),
            ));
        }
        Self::new_inner(
            parent_root,
            session_id,
            worktrees_base,
            command_policy,
            Some(revision),
        )
        .await
    }
}
