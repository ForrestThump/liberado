//! Cross-platform path normalization for child processes.

use std::path::{Path, PathBuf};

/// Return a path that Git and other command-line programs can consume on Windows.
///
/// `std::fs::canonicalize` adds a `\\?\` prefix on Windows. Git for Windows rewrites that
/// prefix as `//?/` and can reject it when it creates a worktree. Native file APIs do not need
/// the prefix for the repository paths Liberado supports, so remove it at the process boundary.
pub fn child_process_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(rest);
    }
    if let Some(rest) = text.strip_prefix("//?/") {
        return PathBuf::from(rest.replace('/', "\\"));
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_process_path_removes_verbatim_drive_and_unc_prefixes() {
        assert_eq!(
            child_process_path(Path::new(r"\\?\C:\Users\me\repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
        assert_eq!(
            child_process_path(Path::new(r"\\?\UNC\server\share\repo")),
            PathBuf::from(r"\\server\share\repo")
        );
        assert_eq!(
            child_process_path(Path::new(r"//?/C:/Users/me/repo")),
            PathBuf::from(r"C:\Users\me\repo")
        );
    }

    #[test]
    fn child_process_path_keeps_forward_slash_drive_paths_unchanged() {
        assert_eq!(
            child_process_path(Path::new("C:/Users/me/repo")),
            PathBuf::from("C:/Users/me/repo")
        );
    }

    #[test]
    fn child_process_path_keeps_plain_paths() {
        assert_eq!(
            child_process_path(Path::new("/home/me/repo")),
            PathBuf::from("/home/me/repo")
        );
    }
}
