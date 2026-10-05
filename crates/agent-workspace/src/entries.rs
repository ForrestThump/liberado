//! Directory listing and contained deletion.
//!
//! A symlink is reported and removed as a symlink. Deleting a tree does not follow it.

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
        return fs::remove_file(path).map_err(WorkspaceError::io);
    }
    if meta.is_dir() {
        return remove_directory(path);
    }
    Err(WorkspaceError::io("unsupported file type"))
}

fn is_file_or_link(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || meta.is_file()
}

fn remove_directory(path: &Path) -> Result<(), WorkspaceError> {
    for entry in read_dir_entries(path)? {
        remove_contained(&entry.path())?;
    }
    fs::remove_dir(path).map_err(WorkspaceError::io)
}
