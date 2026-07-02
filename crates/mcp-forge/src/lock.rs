//! The forge's own build cache: `<install_dir>/.mcp-forge-lock.toml` maps each source's `name` to
//! the git SHA it was last successfully built from. Co-located with the installed binaries (not
//! the config dir) — if the install dir is deleted or moved, the lockfile goes with it, so forge
//! correctly re-treats everything as unbuilt rather than trusting a now-stale record.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

const LOCK_FILE_NAME: &str = ".mcp-forge-lock.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LockFile {
    #[serde(default)]
    built: HashMap<String, String>,
}

impl LockFile {
    /// Load the lockfile from `install_dir`, or an empty one if it doesn't exist yet or fails to
    /// parse — a corrupt/missing lockfile just means "rebuild everything," not a hard error.
    pub fn load(install_dir: &Path) -> Self {
        fs::read_to_string(install_dir.join(LOCK_FILE_NAME))
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, install_dir: &Path) -> std::io::Result<()> {
        fs::create_dir_all(install_dir)?;
        let text = toml::to_string_pretty(self).expect("LockFile always serializes");
        fs::write(install_dir.join(LOCK_FILE_NAME), text)
    }

    pub fn built_sha(&self, name: &str) -> Option<&str> {
        self.built.get(name).map(String::as_str)
    }

    pub fn record(&mut self, name: &str, sha: &str) {
        self.built.insert(name.to_string(), sha.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_lockfile_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let lock = LockFile::load(dir.path());
        assert_eq!(lock.built_sha("weather"), None);
    }

    #[test]
    fn round_trips_through_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut lock = LockFile::load(dir.path());
        lock.record("weather", "abc123");
        lock.save(dir.path()).unwrap();

        let reloaded = LockFile::load(dir.path());
        assert_eq!(reloaded.built_sha("weather"), Some("abc123"));
        assert_eq!(reloaded.built_sha("pdf"), None);
    }

    #[test]
    fn corrupt_lockfile_loads_empty_instead_of_erroring() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(LOCK_FILE_NAME), "not valid toml [[[").unwrap();
        let lock = LockFile::load(dir.path());
        assert_eq!(lock.built_sha("weather"), None);
    }
}
