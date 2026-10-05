//! Resolve a tool path so it stays inside one agent's `files/` directory.
//!
//! Absolute paths, `..`, drive prefixes, backslash separators, and symlinks that leave the
//! directory are refused. A backslash is a separator on every OS, so a model path is accepted or
//! refused the same way on Linux and Windows.
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
    let Some(rel) = parent_components(files, dest)? else {
        return Ok(());
    };
    create_dirs(files, &rel)
}

/// Relative parent of `dest`, or `Ok(None)` when `dest` sits directly in `files`.
fn parent_components(files: &Path, dest: &Path) -> Result<Option<PathBuf>, WorkspaceError> {
    let Some(parent) = dest.parent() else {
        return Err(WorkspaceError::PathEscape);
    };
    if parent == files {
        return Ok(None);
    }
    match parent.strip_prefix(files) {
        Ok(rel) => Ok(Some(rel.to_path_buf())),
        Err(_) => Err(WorkspaceError::PathEscape),
    }
}

fn create_dirs(files: &Path, rel: &Path) -> Result<(), WorkspaceError> {
    let mut acc = files.to_path_buf();
    for component in rel.components() {
        acc.push(normal_name(component)?);
        prepare_dir(&acc)?;
    }
    Ok(())
}

fn normal_name(component: Component<'_>) -> Result<&std::ffi::OsStr, WorkspaceError> {
    match component {
        Component::Normal(name) => Ok(name),
        Component::CurDir | Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
            Err(WorkspaceError::PathEscape)
        }
    }
}

fn prepare_dir(path: &Path) -> Result<(), WorkspaceError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => accept_directory(&meta),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(WorkspaceError::io)
        }
        Err(err) => Err(WorkspaceError::io(err)),
    }
}

fn accept_directory(meta: &fs::Metadata) -> Result<(), WorkspaceError> {
    if meta.file_type().is_symlink() {
        return Err(WorkspaceError::PathEscape);
    }
    if !meta.is_dir() {
        return Err(WorkspaceError::NotADirectory);
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
    // Tool arguments come from a model. On Unix `\` is a filename character, so
    // `..\outside.txt` would be created inside the workspace instead of refused.
    // Treat it as a separator everywhere, matching Windows.
    let normalized = rel.replace('\\', "/");
    let trimmed = normalized.trim();
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
