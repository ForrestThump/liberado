//! Cache of a merge-base CRAP report.
//!
//! The file is `.liberado/crap-base-<sha>.json`. A run reads it only when a touched legacy
//! function is over the CRAP limit. A miss measures, then writes the file. A hit does not
//! measure again.

use std::path::{Path, PathBuf};

pub enum BaseCrapLoad {
    NotNeeded,
    Hit(String),
    Measured(String),
}

impl BaseCrapLoad {
    pub fn label(&self) -> &'static str {
        match self {
            BaseCrapLoad::NotNeeded => "not_needed",
            BaseCrapLoad::Hit(_) => "cache_hit",
            BaseCrapLoad::Measured(_) => "measured",
        }
    }

    pub fn json(&self) -> Option<&str> {
        match self {
            BaseCrapLoad::NotNeeded => None,
            BaseCrapLoad::Hit(text) | BaseCrapLoad::Measured(text) => Some(text),
        }
    }
}

pub fn cache_path(root: &Path, sha: &str) -> PathBuf {
    root.join(".liberado").join(format!("crap-base-{sha}.json"))
}

pub fn read_or_measure(
    path: &Path,
    needed: bool,
    measure: impl FnOnce() -> Result<String, Box<dyn std::error::Error>>,
) -> Result<BaseCrapLoad, Box<dyn std::error::Error>> {
    if !needed {
        return Ok(BaseCrapLoad::NotNeeded);
    }
    if let Some(text) = read_hit(path)? {
        return Ok(BaseCrapLoad::Hit(text));
    }
    let text = measure()?;
    write_cache(path, &text)?;
    Ok(BaseCrapLoad::Measured(text))
}

fn read_hit(path: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    if text.contains("\"entries\"") {
        Ok(Some(text))
    } else {
        Ok(None)
    }
}

fn write_cache(path: &Path, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{cache_path, read_or_measure};
    use std::cell::Cell;

    #[test]
    fn a_run_under_the_limit_does_not_measure() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(dir.path(), "abc");
        let calls = Cell::new(0);
        let loaded = read_or_measure(&path, false, || {
            calls.set(calls.get() + 1);
            Ok("{}".into())
        })
        .unwrap();
        assert_eq!(loaded.label(), "not_needed");
        assert_eq!(calls.get(), 0);
        assert!(loaded.json().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn a_miss_measures_once_and_writes_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_path(dir.path(), "abc");
        let calls = Cell::new(0);
        let loaded = read_or_measure(&path, true, || {
            calls.set(calls.get() + 1);
            Ok("{\"entries\":[]}\n".into())
        })
        .unwrap();
        assert_eq!(loaded.label(), "measured");
        assert_eq!(calls.get(), 1);
        assert!(path.is_file());
        let again = read_or_measure(&path, true, || {
            calls.set(calls.get() + 1);
            Ok("nope".into())
        })
        .unwrap();
        assert_eq!(again.label(), "cache_hit");
        assert_eq!(calls.get(), 1);
        assert!(again.json().unwrap().contains("entries"));
    }
}
