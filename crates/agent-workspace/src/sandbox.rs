//! Resolve a tool path so it stays inside one agent's `files/` directory.
//!
//! Absolute paths, `..`, drive prefixes, and symlinks that leave the directory are refused.
//! A backslash is normalized to `/` before the path is checked, so it is a separator on every
//! OS and a model path is accepted or refused the same way on Linux and Windows.
//! Parent directory symlinks are followed so containment is checked. The final component is
//! not followed: delete and write operate on that name itself. Read and list use
//! [`resolve_following`] and still refuse a final symlink whose target leaves the directory.
//! Missing parents are created one component at a time, and only after the byte cap has been
//! checked by the caller. `create_dir_all` is not used: it would follow a symlink out of the
//! workspace.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::error::WorkspaceError;

pub(crate) fn resolve(files: &Path, rel: &str) -> Result<PathBuf, WorkspaceError> {
    let relative = relative_path(rel)?;
    if relative.as_os_str().is_empty() {
        return Ok(files.to_path_buf());
    }
    contained(files, walk_components(files, &relative)?)
}

/// [`resolve`], then follow a final symlink that stays inside `files`.
pub(crate) fn resolve_following(files: &Path, rel: &str) -> Result<PathBuf, WorkspaceError> {
    let path = resolve(files, rel)?;
    match follow_symlink(files, &path)? {
        Some(resolved) => contained(files, resolved),
        None => Ok(path),
    }
}

/// Follow every parent. Leave the last name as it is, even when it is a symlink.
fn walk_components(files: &Path, relative: &Path) -> Result<PathBuf, WorkspaceError> {
    let mut names = normal_names(relative);
    let Some(last) = names.pop() else {
        return Ok(files.to_path_buf());
    };
    let mut acc = files.to_path_buf();
    for name in names {
        acc = push_followed(files, acc, name)?;
    }
    Ok(push_final(acc, last))
}

fn normal_names(relative: &Path) -> Vec<&OsStr> {
    let mut names = Vec::new();
    for component in relative.components() {
        if let Component::Normal(name) = component {
            names.push(name);
        }
    }
    names
}

fn push_followed(files: &Path, mut acc: PathBuf, name: &OsStr) -> Result<PathBuf, WorkspaceError> {
    acc.push(name);
    match follow_symlink(files, &acc)? {
        Some(resolved) => Ok(resolved),
        None => Ok(acc),
    }
}

fn push_final(mut acc: PathBuf, name: &OsStr) -> PathBuf {
    acc.push(name);
    acc
}

/// Create missing directories under `files` for `dest`'s parent.
///
/// `dest` is refused before any directory is created unless every component after `files` is a
/// normal file name. `.` and `..` are refused even when a verbatim path parses them as normal
/// names. A symlink component is refused rather than followed.
pub(crate) fn ensure_parents(files: &Path, dest: &Path) -> Result<(), WorkspaceError> {
    require_plain_dest(files, dest)?;
    let Some(rel) = parent_components(files, dest)? else {
        return Ok(());
    };
    create_dirs(files, &rel)
}

/// Refuse `dest` unless it names a file strictly inside `files`.
///
/// Components come from `dest` itself. A `\\?\` path can report `.` and `..` as
/// [`Component::Normal`]; [`parent`](Path::parent) would drop that final name before
/// [`normal_name`] sees it.
fn require_plain_dest(files: &Path, dest: &Path) -> Result<(), WorkspaceError> {
    let mut rest = dest.components();
    for expected in files.components() {
        if rest.next() != Some(expected) {
            return Err(WorkspaceError::PathEscape);
        }
    }
    accept_plain_file(rest)
}

fn accept_plain_file(rest: std::path::Components<'_>) -> Result<(), WorkspaceError> {
    let mut saw_name = false;
    for component in rest {
        if !plain_name(component) {
            return Err(WorkspaceError::PathEscape);
        }
        saw_name = true;
    }
    if saw_name {
        Ok(())
    } else {
        Err(WorkspaceError::PathEscape)
    }
}

fn plain_name(component: Component<'_>) -> bool {
    match component {
        Component::Normal(name) => !dot_name(name),
        _ => false,
    }
}

fn dot_name(name: &OsStr) -> bool {
    name == "." || name == ".."
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

fn normal_name(component: Component<'_>) -> Result<&OsStr, WorkspaceError> {
    match component {
        Component::Normal(name) if !dot_name(name) => Ok(name),
        Component::Normal(_)
        | Component::CurDir
        | Component::ParentDir
        | Component::RootDir
        | Component::Prefix(_) => Err(WorkspaceError::PathEscape),
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

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Component, Path};

    use super::{create_dirs, dot_name, normal_name, parent_components, plain_name};

    #[test]
    fn dot_names_are_not_plain_file_names() {
        assert!(dot_name(OsStr::new(".")));
        assert!(dot_name(OsStr::new("..")));
        assert!(!dot_name(OsStr::new("note.txt")));
        assert!(!plain_name(Component::Normal(OsStr::new("."))));
        assert!(!plain_name(Component::Normal(OsStr::new(".."))));
        assert!(plain_name(Component::Normal(OsStr::new("note.txt"))));
        assert!(!plain_name(Component::ParentDir));
        assert!(normal_name(Component::Normal(OsStr::new(".."))).is_err());
        assert!(normal_name(Component::Normal(OsStr::new("."))).is_err());
        assert!(normal_name(Component::ParentDir).is_err());
        assert_eq!(
            normal_name(Component::Normal(OsStr::new("note.txt"))).unwrap(),
            "note.txt"
        );
    }

    #[test]
    fn parent_components_rejects_a_root_and_a_sibling() {
        let files = Path::new("/workspace/files");
        assert!(parent_components(files, Path::new("/")).is_err());
        assert!(parent_components(files, Path::new("/workspace/note.txt")).is_err());
        assert!(
            parent_components(files, &files.join("note.txt"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            parent_components(files, &files.join("a").join("b.txt"))
                .unwrap()
                .unwrap(),
            Path::new("a")
        );
        // `..` is refused before `create_dirs` touches the filesystem.
        assert!(create_dirs(files, Path::new("..")).is_err());
    }
}
