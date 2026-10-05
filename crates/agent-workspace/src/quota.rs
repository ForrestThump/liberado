//! Byte cap for one agent's files.
//!
//! Every file, directory, and symlink under `files/` costs [`ENTRY_COST`] bytes. A regular file
//! also costs its length. Symlinks are not followed and do not add the target's size. A new file
//! or new parent directory must fit that entry cost before it is created. Cap 0 refuses every
//! write and download, including an empty file. A replace subtracts the old file's length before
//! adding the new bytes; the entry cost of an existing path stays.

use std::fs;
use std::path::Path;

use crate::error::WorkspaceError;

/// Bytes charged for one file, directory, or symlink, before its content length.
pub(crate) const ENTRY_COST: u64 = 4096;

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
        *total = total.saturating_add(ENTRY_COST);
        if link_entry(&meta) {
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

fn link_entry(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || crate::entries::is_dir_link(meta)
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

/// Admit `new_bytes` of file content plus the cost of any path component this write would create.
pub(crate) fn admit_write(
    files: &Path,
    dest: &Path,
    new_bytes: u64,
    cap: u64,
) -> Result<(), WorkspaceError> {
    let (used, old, overhead) = charge(files, dest)?;
    admit(used, old, new_bytes.saturating_add(overhead), cap)
}

/// Pre-check a download, including an empty or length-less body.
///
/// Returns `(used + overhead, old content length)` so the streaming admit can keep its formula.
/// `announced == None` still pays the entry cost, which is what makes cap 0 refuse an empty file.
pub(crate) fn gate_download(
    files: &Path,
    dest: &Path,
    announced: Option<u64>,
    cap: u64,
) -> Result<(u64, u64), WorkspaceError> {
    let (used, old, overhead) = charge(files, dest)?;
    let announced = announced.unwrap_or(0);
    admit(used, old, announced.saturating_add(overhead), cap)?;
    Ok((used.saturating_add(overhead), old))
}

fn charge(files: &Path, dest: &Path) -> Result<(u64, u64, u64), WorkspaceError> {
    let used = usage(files)?;
    let old = replaced_bytes(dest)?;
    let overhead = new_entry_cost(files, dest)?;
    Ok((used, old, overhead))
}

fn new_entry_cost(files: &Path, dest: &Path) -> Result<u64, WorkspaceError> {
    Ok(ENTRY_COST.saturating_mul(missing_components(files, dest)?))
}

fn missing_components(files: &Path, dest: &Path) -> Result<u64, WorkspaceError> {
    let rel = dest
        .strip_prefix(files)
        .map_err(|_| WorkspaceError::PathEscape)?;
    let mut cursor = files.to_path_buf();
    let mut missing = 0u64;
    for component in rel.components() {
        cursor.push(component);
        missing = missing.saturating_add(u64::from(entry_absent(&cursor)?));
    }
    Ok(missing)
}

fn entry_absent(path: &Path) -> Result<bool, WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(err) => Err(WorkspaceError::io(err)),
    }
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
