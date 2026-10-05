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

/// Bytes a replace subtracts. A symlink or junction contributes no file bytes.
/// A missing path contributes nothing. A real directory is not a file.
pub(crate) fn replaced_bytes(path: &Path) -> Result<u64, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => bytes_if_replaced(&meta),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(WorkspaceError::io(err)),
    }
}

fn bytes_if_replaced(meta: &fs::Metadata) -> Result<u64, WorkspaceError> {
    if replaced_link(meta) {
        return Ok(0);
    }
    if meta.is_file() {
        return Ok(meta.len());
    }
    Err(WorkspaceError::NotAFile)
}

fn replaced_link(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || crate::entries::is_dir_link(meta)
}
