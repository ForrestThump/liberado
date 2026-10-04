//! Byte cap for one agent's files.
//!
//! Usage is the sum of regular file lengths under `files/`. Symlinks are not followed and do not
//! add the target's size. A replace subtracts the old file before adding the new bytes.

use std::fs;
use std::path::Path;

use crate::error::WorkspaceError;

pub(crate) fn usage(files: &Path) -> Result<u64, WorkspaceError> {
    let mut total = 0u64;
    walk(files, &mut total)?;
    Ok(total)
}

fn walk(path: &Path, total: &mut u64) -> Result<(), WorkspaceError> {
    let entries = fs::read_dir(path).map_err(WorkspaceError::io)?;
    for entry in entries {
        let entry = entry.map_err(WorkspaceError::io)?;
        let meta = fs::symlink_metadata(entry.path()).map_err(WorkspaceError::io)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(&entry.path(), total)?;
        } else if meta.is_file() {
            *total = total.saturating_add(meta.len());
        }
    }
    Ok(())
}

/// `used` includes `old_len` when the destination file already exists.
pub(crate) fn admit(used: u64, old_len: u64, new_len: u64, cap: u64) -> Result<(), WorkspaceError> {
    let would = used.saturating_sub(old_len).saturating_add(new_len);
    if would > cap {
        return Err(WorkspaceError::OverCap {
            adding: new_len,
            would_use: would,
            cap,
        });
    }
    Ok(())
}

pub(crate) fn file_len(path: &Path) -> Result<u64, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(meta.len()),
        Ok(_) => Err(WorkspaceError::NotAFile),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(WorkspaceError::io(err)),
    }
}
