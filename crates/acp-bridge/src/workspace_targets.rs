//! Apply the managed ordinary Cargo cache to one ACP coding job.
//!
//! Call this with the session's project root (the client cwd) on every coding
//! prompt, including a reused converse handle. Coverage, mutation, and
//! comparison jobs do not use this process env.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use liberado_coder_core::WorkspaceBuildConfig;

/// Last `CARGO_TARGET_DIR` this process applied. Worktree-local or failed
/// allocation clears that value so session B cannot leak into session A.
static LAST_APPLIED: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Point child processes at the ordinary cache for `source_root`.
///
/// `source_root` is the repo this job is of. Durable worktrees from that root
/// share one allocation. A different client cwd gets a different hash.
///
/// A worktree-local result or allocation error removes a value this function
/// previously set. An operator-exported `CARGO_TARGET_DIR` that we never
/// overwrote stays in place.
pub fn apply_workspace_targets(build: &WorkspaceBuildConfig, source_root: &Path) {
    match liberado_coder_sandbox::resolve_ordinary(build, source_root) {
        Ok(allocation) if allocation.kind != liberado_coder_sandbox::TargetKind::WorktreeLocal => {
            unsafe { std::env::set_var("CARGO_TARGET_DIR", &allocation.path) };
            remember_applied(Some(allocation.path.clone()));
            tracing::info!(
                target_dir = %allocation.path.display(),
                kind = ?allocation.kind,
                "coding worktrees share one cargo build cache"
            );
        }
        Ok(_) => {
            clear_applied_target();
        }
        Err(error) => {
            clear_applied_target();
            tracing::warn!(
                error = %error,
                "managed cargo target not applied; children keep worktree-local target/"
            );
        }
    }
}

fn remember_applied(path: Option<PathBuf>) {
    *LAST_APPLIED.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

fn clear_applied_target() {
    let mut last = LAST_APPLIED.lock().unwrap_or_else(|e| e.into_inner());
    let current = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    if current.as_ref() == last.as_ref() {
        unsafe { std::env::remove_var("CARGO_TARGET_DIR") };
    }
    *last = None;
}
