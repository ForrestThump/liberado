//! Apply the managed ordinary Cargo cache to one ACP coding job.
//!
//! Call this with the session's project root (the client cwd), after the
//! worktree exists, not with the bridge process CWD at startup. Coverage,
//! mutation, and comparison jobs do not use this process env.

use std::path::Path;

use liberado_coder_core::WorkspaceBuildConfig;

/// Point child processes at the ordinary cache for `source_root`.
///
/// `source_root` is the repo this job is of. Durable worktrees from that root
/// share one allocation. A different client cwd gets a different hash.
pub fn apply_workspace_targets(build: &WorkspaceBuildConfig, source_root: &Path) {
    match liberado_coder_sandbox::resolve_ordinary(build, source_root) {
        Ok(allocation) if allocation.kind != liberado_coder_sandbox::TargetKind::WorktreeLocal => {
            unsafe { std::env::set_var("CARGO_TARGET_DIR", &allocation.path) };
            tracing::info!(
                target_dir = %allocation.path.display(),
                kind = ?allocation.kind,
                "coding worktrees share one cargo build cache"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(
            error = %error,
            "managed cargo target not applied; children keep worktree-local target/"
        ),
    }
}
