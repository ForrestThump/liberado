//! Shared-cache attach: leftover lock checks, then class stamp.
//!
//! Lives in its own file so `cargo_targets.rs` stays under the new-file
//! module-health ceiling. Isolated exclusive locks stay in the parent.

use std::path::Path;

use super::{
    TargetClass, TargetError, lock_holder_alive, lock_path_for, read_lock_class, stamp_class,
};

/// Attach `class` to a shared target. A leftover lock of another class is
/// refused. A lock of this class may be shared. An unreadable lock with a
/// live holder is busy; a dead or pid-0 lock is ignored and the stamp wins.
pub(super) fn attach_shared(path: &Path, class: TargetClass) -> Result<(), TargetError> {
    refuse_shared_lock_conflict(path, class)?;
    stamp_class(path, class)
}

fn refuse_shared_lock_conflict(path: &Path, class: TargetClass) -> Result<(), TargetError> {
    let lock = lock_path_for(path);
    if !lock.is_file() {
        return Ok(());
    }
    refuse_existing_shared_lock(&lock, path, class)
}

fn refuse_existing_shared_lock(
    lock: &Path,
    path: &Path,
    class: TargetClass,
) -> Result<(), TargetError> {
    let Some(existing) = read_lock_class(lock) else {
        return refuse_if_holder_alive(lock, path, class);
    };
    if existing != class {
        return Err(incompatible_class(path, existing, class));
    }
    Ok(())
}

fn refuse_if_holder_alive(lock: &Path, path: &Path, class: TargetClass) -> Result<(), TargetError> {
    if !lock_holder_alive(lock) {
        return Ok(());
    }
    Err(TargetError::Busy {
        path: path.to_path_buf(),
        class,
    })
}

fn incompatible_class(path: &Path, existing: TargetClass, requested: TargetClass) -> TargetError {
    TargetError::Incompatible {
        path: path.to_path_buf(),
        existing,
        requested,
    }
}
