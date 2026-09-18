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
                 (enable product feature `coder-full` on liberado-cli / liberado-server, \
                 use liberado-coder-run / liberado-coder-runner, or build with --features git)"
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

    #[cfg(test)]
    mod stub_tests {
        use super::*;
        use std::path::Path;

        #[test]
        fn status_mentions_coder_full_and_runner() {
            let err = status(Path::new(".")).unwrap_err();
            assert!(
                err.message.contains("coder-full"),
                "stub should mention product feature coder-full: {}",
                err.message
            );
            assert!(
                err.message.contains("liberado-coder-run"),
                "stub should mention liberado-coder-run: {}",
                err.message
            );
            assert!(
                err.message.contains("unavailable"),
                "stub should say unavailable: {}",
                err.message
            );
        }

        #[test]
        fn diff_and_commit_stubs_share_guidance() {
            for (op, res) in [
                ("diff", diff_name_only(Path::new(".")).unwrap_err()),
                ("commit", commit(Path::new("."), "msg", None).unwrap_err()),
                ("branch", branch_create(Path::new("."), "x").unwrap_err()),
            ] {
                assert!(
                    res.message.contains("coder-full")
                        && res.message.contains("liberado-coder-run"),
                    "{op} stub missing product guidance: {}",
                    res.message
                );
            }
        }
    }
}

pub use imp::*;
