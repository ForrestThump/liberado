//! Coding-pack git tools.
//!
//! Real implementation (`git_gix`) is behind the `git` Cargo feature (gix).
//! Without that feature the same public surface returns a clear error so the
//! default liberado server/cli graph does not link gix.

#[cfg(feature = "git")]
#[path = "git_gix.rs"]
mod imp;

#[cfg(not(feature = "git"))]
mod imp {
    use std::path::Path;

    #[derive(Debug, Clone)]
    pub struct GitError {
        pub exit_code: i32,
        pub message: String,
    }

    impl std::fmt::Display for GitError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.message)
        }
    }

    impl std::error::Error for GitError {}

    fn unavailable(op: &str) -> GitError {
        GitError {
            exit_code: 1,
            message: format!(
                "git_{op} unavailable: liberado-coder-tools built without the `git` feature \
                 (build with --features git, or use liberado-coder-run which enables full tools)"
            ),
        }
    }

    pub fn status(_root: &Path) -> Result<String, GitError> {
        Err(unavailable("status"))
    }

    pub fn diff_name_only(_root: &Path) -> Result<String, GitError> {
        Err(unavailable("diff"))
    }

    pub fn diff_stat(_root: &Path) -> Result<String, GitError> {
        Err(unavailable("diff"))
    }

    pub fn diff_patch(_root: &Path) -> Result<String, GitError> {
        Err(unavailable("diff"))
    }

    pub fn untracked_files(_root: &Path) -> Result<Vec<String>, GitError> {
        Err(unavailable("diff"))
    }

    pub fn branch_create(_root: &Path, _name: &str) -> Result<(), GitError> {
        Err(unavailable("branch"))
    }

    pub fn commit(
        _root: &Path,
        _message: &str,
        _files: Option<&[String]>,
    ) -> Result<String, GitError> {
        Err(unavailable("commit"))
    }

    pub fn push(
        _root: &Path,
        _remote: &str,
        _branch: Option<&str>,
        _set_upstream: bool,
    ) -> Result<String, GitError> {
        Err(unavailable("push"))
    }

    pub fn log(
        _root: &Path,
        _limit: u32,
        _fmt: Option<&str>,
        _branch: Option<&str>,
    ) -> Result<String, GitError> {
        Err(unavailable("log"))
    }

    pub fn fetch(_root: &Path, _remote: &str, _branch: Option<&str>) -> Result<String, GitError> {
        Err(unavailable("fetch"))
    }

    pub fn merge(_root: &Path, _branch: &str, _ff_only: bool) -> Result<String, GitError> {
        Err(unavailable("merge"))
    }
}

pub use imp::*;
