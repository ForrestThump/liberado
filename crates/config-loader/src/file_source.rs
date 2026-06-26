//! A [`ConfigSource`] that reads TOML content from a file on disk.
//!
//! A missing file yields `Ok(None)` (not an error), matching the convention that
//! every config file is optional.

use std::path::PathBuf;

use crate::source::{ConfigLoadError, ConfigSource};

/// A [`ConfigSource`] backed by a single file.
///
/// Returns `Ok(None)` when the file does not exist, `Err` on other I/O errors
/// (permissions, broken symlink, …).
///
/// # Example
///
/// ```rust
/// use liberado_config_loader::{ConfigSource, FileSource};
/// use std::path::PathBuf;
///
/// let source = FileSource::new(PathBuf::from("/etc/liberado/topology.toml"));
/// // source.load_raw() -> Ok(Some("…")) or Ok(None) or Err(…)
/// ```
#[derive(Debug)]
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    /// Create a new source that reads from `path`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The path this source reads from.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl ConfigSource for FileSource {
    fn load_raw(&self) -> Result<Option<String>, ConfigLoadError> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => Ok(Some(content)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigLoadError::Io {
                source: self.path.display().to_string(),
                inner: e,
            }),
        }
    }

    fn description(&self) -> &str {
        self.path.to_str().unwrap_or("<non-utf8 path>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_existing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"key = \"value\"").unwrap();
        drop(f);

        let source = FileSource::new(path);
        let result = source.load_raw().expect("load should succeed");
        assert_eq!(result, Some("key = \"value\"".to_string()));
    }

    #[test]
    fn missing_file_yields_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");

        let source = FileSource::new(path);
        let result = source.load_raw().expect("missing file should not error");
        assert!(result.is_none());
    }

    #[test]
    fn description_returns_file_path() {
        let source = FileSource::new(PathBuf::from("/tmp/my-config.toml"));
        assert_eq!(source.description(), "/tmp/my-config.toml");
    }

    #[test]
    fn description_does_not_panic() {
        let source = FileSource::new(PathBuf::from("./config.toml"));
        let _desc = source.description();
    }
}
