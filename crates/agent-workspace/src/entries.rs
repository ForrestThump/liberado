//! Directory listing and contained deletion.
//!
//! A symlink is reported as a symlink and unlinked without following it. Deleting a tree does
//! not walk through a symlink. On Windows a directory symlink or junction is removed with
//! `remove_dir`, which drops the link itself; the target is left in place.

use std::fs;
use std::path::Path;

use crate::error::WorkspaceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub bytes: u64,
}

pub(crate) fn list_directory(path: &Path) -> Result<Vec<DirEntry>, WorkspaceError> {
    require_directory(path)?;
    let mut entries = collect_entries(path)?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn require_directory(path: &Path) -> Result<(), WorkspaceError> {
    let meta = fs::symlink_metadata(path).map_err(WorkspaceError::missing)?;
    if meta.is_dir() {
        Ok(())
    } else {
        Err(WorkspaceError::NotADirectory)
    }
}

fn collect_entries(path: &Path) -> Result<Vec<DirEntry>, WorkspaceError> {
    let mut entries = Vec::new();
    for entry in read_dir_entries(path)? {
        entries.push(dir_entry(&entry)?);
    }
    Ok(entries)
}

fn read_dir_entries(path: &Path) -> Result<Vec<fs::DirEntry>, WorkspaceError> {
    fs::read_dir(path)
        .map_err(WorkspaceError::io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(WorkspaceError::io)
}

fn dir_entry(entry: &fs::DirEntry) -> Result<DirEntry, WorkspaceError> {
    let meta = fs::symlink_metadata(entry.path()).map_err(WorkspaceError::io)?;
    let kind = entry_kind(&meta);
    Ok(DirEntry {
        name: entry.file_name().to_string_lossy().into_owned(),
        kind,
        bytes: bytes_of(kind, &meta),
    })
}

fn entry_kind(meta: &fs::Metadata) -> EntryKind {
    if meta.file_type().is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

fn bytes_of(kind: EntryKind, meta: &fs::Metadata) -> u64 {
    if kind == EntryKind::File {
        meta.len()
    } else {
        0
    }
}

pub(crate) fn remove_contained(path: &Path) -> Result<(), WorkspaceError> {
    let meta = fs::symlink_metadata(path).map_err(WorkspaceError::missing)?;
    if is_file_or_link(&meta) {
        return remove_file_or_link(path, &meta);
    }
    if meta.is_dir() {
        return remove_directory(path);
    }
    Err(WorkspaceError::io("unsupported file type"))
}

fn is_file_or_link(meta: &fs::Metadata) -> bool {
    removable_without_recursion(meta)
}

fn removable_without_recursion(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || meta.is_file() || is_dir_link(meta)
}

/// A directory symlink or junction. `remove_file` cannot unlink those on Windows.
/// Detected from `symlink_metadata`, so the target is not opened.
pub(crate) fn is_dir_link(meta: &fs::Metadata) -> bool {
    dir_link(meta)
}

#[cfg(windows)]
fn dir_link(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x0000_0400;
    const DIRECTORY: u32 = 0x0000_0010;
    let attrs = meta.file_attributes();
    attrs & REPARSE_POINT != 0 && attrs & DIRECTORY != 0
}

#[cfg(not(windows))]
fn dir_link(_meta: &fs::Metadata) -> bool {
    false
}

fn remove_file_or_link(path: &Path, meta: &fs::Metadata) -> Result<(), WorkspaceError> {
    if is_dir_link(meta) {
        fs::remove_dir(path).map_err(WorkspaceError::io)
    } else {
        fs::remove_file(path).map_err(WorkspaceError::io)
    }
}

/// Refuse a real directory. A symlink or junction is not a real directory: callers
/// replace the link instead of writing through it.
pub(crate) fn reject_real_dir(path: &Path) -> Result<(), WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if real_dir(&meta) => Err(WorkspaceError::NotAFile),
        _ => Ok(()),
    }
}

fn real_dir(meta: &fs::Metadata) -> bool {
    meta.is_dir() && !meta.file_type().is_symlink() && !is_dir_link(meta)
}

/// Replace `path` with a regular file. A symlink or junction is unlinked first so
/// the write does not open the target.
pub(crate) fn write_replacing_link(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    unlink_if_link(path)?;
    fs::write(path, bytes).map_err(WorkspaceError::io)
}

fn unlink_if_link(path: &Path) -> Result<(), WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if removable_link(&meta) => remove_file_or_link(path, &meta),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(WorkspaceError::io(err)),
    }
}

fn removable_link(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || is_dir_link(meta)
}

/// Move `from` onto `to`.
///
/// `rename` replaces a file or a symlink where the platform allows it. Where it does
/// not (Windows, when the destination exists), the final component is removed and the
/// rename is retried. A real directory is left in place.
pub(crate) fn replace_file(from: &Path, to: &Path) -> Result<(), WorkspaceError> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) => replace_existing(from, to, err),
    }
}

fn replace_existing(from: &Path, to: &Path, err: std::io::Error) -> Result<(), WorkspaceError> {
    if !final_exists(to) {
        return Err(WorkspaceError::io(err));
    }
    remove_final(to)?;
    fs::rename(from, to).map_err(WorkspaceError::io)
}

fn final_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn remove_final(path: &Path) -> Result<(), WorkspaceError> {
    let meta = fs::symlink_metadata(path).map_err(WorkspaceError::missing)?;
    if removable_without_recursion(&meta) {
        return remove_file_or_link(path, &meta);
    }
    Err(WorkspaceError::NotAFile)
}

fn remove_directory(path: &Path) -> Result<(), WorkspaceError> {
    for entry in read_dir_entries(path)? {
        remove_contained(&entry.path())?;
    }
    fs::remove_dir(path).map_err(WorkspaceError::io)
}
