//! Apply the managed ordinary Cargo cache to ACP coding runs.

use std::path::Path;

use liberado_coder_core::WorkspaceBuildConfig;

/// Point child processes at the managed ordinary cache.
///
/// Safe at single-threaded startup. Coverage, mutation, and comparison jobs
/// do not use this process env; they keep isolated targets.
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
