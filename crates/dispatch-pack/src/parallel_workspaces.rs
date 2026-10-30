//! Worktree roots for one parallel dispatch.
//!
//! The sandbox types stay in this module so `lib.rs` keeps its function-count baseline.
//! Dropping the worktrees before `dispatch_parallel` returns would remove the directories
//! the workers are using, so the values stay alive inside [`PreparedParallel`].

use std::path::{Path, PathBuf};

#[cfg(feature = "coding")]
use liberado_coder_core::CommandPolicy;
#[cfg(feature = "coding")]
use liberado_coder_sandbox::{Workspace, WorktreeWorkspace};
use liberado_session::PackError;

/// Worktrees created for one parallel dispatch.
pub(super) struct PreparedParallel {
    #[cfg(feature = "coding")]
    _worktrees: Vec<Option<WorktreeWorkspace>>,
    pub(super) roots: Vec<Option<PathBuf>>,
}

/// Build one workspace root per sub-goal.
///
/// No `parent_root` means shared-nothing: every root is `None` and no sandbox type is named.
/// A `parent_root` without the `coding` feature is a hard error. There is no shared-workspace
/// fallback.
pub(super) async fn prepare_parallel_workspaces(
    parent_root: Option<&Path>,
    count: usize,
    correlation_id: &str,
    worktrees_base: &Path,
) -> Result<PreparedParallel, PackError> {
    #[cfg(not(feature = "coding"))]
    {
        let _ = (correlation_id, worktrees_base);
        if parent_root.is_some() {
            Err(PackError::Failed(
                "built without the `coding` feature".into(),
            ))
        } else {
            Ok(PreparedParallel {
                roots: vec![None; count],
            })
        }
    }
    #[cfg(feature = "coding")]
    {
        let mut worktrees = Vec::with_capacity(count);
        let mut roots = Vec::with_capacity(count);
        if let Some(parent) = parent_root {
            for i in 0..count {
                let wt_session = format!("dp-{correlation_id}-{i}");
                let ws = WorktreeWorkspace::new(
                    parent,
                    &wt_session,
                    worktrees_base,
                    CommandPolicy::default(),
                )
                .await
                .map_err(|e| {
                    PackError::Failed(format!("worktree for parallel sub-goal {i}: {e}"))
                })?;
                roots.push(Some(ws.root().to_path_buf()));
                worktrees.push(Some(ws));
            }
        } else {
            worktrees.resize_with(count, || None);
            roots.resize_with(count, || None);
        }
        Ok(PreparedParallel {
            _worktrees: worktrees,
            roots,
        })
    }
}
