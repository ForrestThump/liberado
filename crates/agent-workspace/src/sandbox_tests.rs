//! Dot-component checks for `ensure_parents`.
//!
//! Split from `tests.rs` so that file stays within the module-health function
//! boundary. `append_component` is shared with the escape test there.

use std::path::Path;

/// Append `component` as raw text. `PathBuf::push("..")` on a canonical `\\?\` path deletes
/// the previous component, so the `..` would never reach `ensure_parents`.
pub(crate) fn append_component(path: &Path, component: &str) -> std::path::PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(std::path::MAIN_SEPARATOR_STR);
    raw.push(component);
    std::path::PathBuf::from(raw)
}

#[cfg(windows)]
fn open(root: &Path, agent_id: &str, cap: u64) -> crate::workspace::AgentWorkspace {
    crate::workspace::AgentWorkspace::open(root, agent_id, cap).unwrap()
}

#[cfg(windows)]
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// A canonical Windows path uses the `\\?\` prefix. `.` and `..` in that prefix are still refused,
/// including when the parser reports them as ordinary names.
#[cfg(windows)]
#[test]
fn ensure_parents_rejects_dot_components_in_a_verbatim_path() {
    use crate::WorkspaceError;

    let root = scratch();
    let workspace = open(root.path(), "boxed", 100);
    let files = workspace.files_dir().to_path_buf();
    let verbatim = files.to_string_lossy();
    assert!(
        verbatim.starts_with(r"\\?\"),
        "canonicalize should yield a verbatim path, got {verbatim}"
    );

    let escaped = append_component(
        &append_component(&append_component(&files, "sub"), ".."),
        "note.txt",
    );
    let err = crate::sandbox::ensure_parents(&files, &escaped).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");
    assert!(!files.join("sub").exists());

    let dotted = append_component(&append_component(&files, "."), "note.txt");
    let err = crate::sandbox::ensure_parents(&files, &dotted).unwrap_err();
    assert!(matches!(err, WorkspaceError::PathEscape), "{err:?}");

    let kept = append_component(&append_component(&files, "kept"), "note.txt");
    crate::sandbox::ensure_parents(&files, &kept).unwrap();
    assert!(files.join("kept").is_dir());
}
