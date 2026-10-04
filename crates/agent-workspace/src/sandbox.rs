//! Resolve a tool path so it stays inside one agent's `files/` directory.
//!
//! Absolute paths, `..`, drive prefixes, and symlinks that leave the directory are refused.
//! Missing parents are created one component at a time, and only after the byte cap has been
//! checked by the caller. `create_dir_all` is not used: it would follow a symlink out of the
//! workspace.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::WorkspaceError;

pub(crate) fn resolve(files: &Path, rel: &str) -> Result<PathBuf, WorkspaceError> {
    let relative = relative_path(rel)?;
    if relative.as_os_str().is_empty() {
        return Ok(files.to_path_buf());
    }
    let mut acc = files.to_path_buf();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                acc.push(name);
                if let Some(resolved) = follow_symlink(files, &acc)? {
                    acc = resolved;
                }
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkspaceError::PathEscape);
            }
        }
    }
    contained(files, acc)
}

/// Create missing directories under `files` for `dest`'s parent. A symlink component is refused
/// rather than followed.
pub(crate) fn ensure_parents(files: &Path, dest: &Path) -> Result<(), WorkspaceError> {
    let Some(parent) = dest.parent() else {
        return Err(WorkspaceError::PathEscape);
    };
    if parent == files {
        return Ok(());
    }
    let rel = parent
        .strip_prefix(files)
        .map_err(|_| WorkspaceError::PathEscape)?;
    let mut acc = files.to_path_buf();
    for component in rel.components() {
        let Component::Normal(name) = component else {
            return Err(WorkspaceError::PathEscape);
        };
        acc.push(name);
        match fs::symlink_metadata(&acc) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(WorkspaceError::PathEscape),
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(WorkspaceError::NotADirectory),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&acc).map_err(WorkspaceError::io)?;
            }
            Err(err) => return Err(WorkspaceError::io(err)),
        }
    }
    Ok(())
}

fn relative_path(rel: &str) -> Result<PathBuf, WorkspaceError> {
    if rel.as_bytes().contains(&0) {
        return Err(WorkspaceError::BadPath("NUL".into()));
    }
    if rel.contains(':') {
        return Err(WorkspaceError::PathEscape);
    }
    let trimmed = rel.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(PathBuf::new());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(WorkspaceError::PathEscape);
    }
    for component in path.components() {
        match component {
            Component::Normal(name) => reject_alias(name)?,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkspaceError::PathEscape);
            }
        }
    }
    Ok(path.to_path_buf())
}

fn reject_alias(name: &std::ffi::OsStr) -> Result<(), WorkspaceError> {
    let text = name.to_string_lossy();
    if text.ends_with('.') || text.ends_with(' ') {
        return Err(WorkspaceError::BadPath(
            "a name cannot end with a dot or a space".into(),
        ));
    }
    Ok(())
}

/// If `path` is a symlink, return its canonical target. A target outside `files` is an error.
fn follow_symlink(files: &Path, path: &Path) -> Result<Option<PathBuf>, WorkspaceError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(WorkspaceError::io(err)),
    };
    if !meta.file_type().is_symlink() {
        return Ok(None);
    }
    let canon = fs::canonicalize(path).map_err(WorkspaceError::io)?;
    if !canon.starts_with(files) {
        return Err(WorkspaceError::PathEscape);
    }
    Ok(Some(canon))
}

fn contained(files: &Path, path: PathBuf) -> Result<PathBuf, WorkspaceError> {
    if path.starts_with(files) {
        Ok(path)
    } else {
        Err(WorkspaceError::PathEscape)
    }
}
