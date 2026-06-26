//! The [`ChainLoader`]: merges multiple [`ConfigSource`]s in precedence order.
//!
//! Sources are evaluated in insertion order. Each source that returns content is
//! deserialized into a [`toml::Value`] and merged: later sources override earlier
//! ones at the TOML key level (tables are merged recursively, scalars and arrays
//! are replaced wholesale).
//!
//! When no source provides any content, the loader returns `None` (or the target
//! type's [`Default`] when using [`ChainLoader::load`]).

use serde::de::DeserializeOwned;

use crate::source::{ConfigLoadError, ConfigSource};

/// Chains multiple [`ConfigSource`]s in precedence order and merges their content.
///
/// Higher-priority sources (added later) override lower-priority sources at the TOML
/// table/key level. Tables are merged recursively; all other values (scalars, arrays)
/// are replaced by the higher-priority source.
///
/// # Examples
///
/// ```rust
/// use liberado_config_loader::{ChainLoader, ConfigSource, FileSource, ConfigLoadError};
/// use std::path::PathBuf;
///
/// // Chain two file sources: system defaults then local overrides.
/// let loader = ChainLoader::new()
///     .add_source(Box::new(FileSource::new(PathBuf::from("/etc/liberado/topology.toml"))))
///     .add_source(Box::new(FileSource::new(PathBuf::from("./config/topology.toml"))));
///
/// // Load as a toml::Value to inspect the merged result.
/// let merged = loader.load_value().expect("load should succeed");
/// ```
///
/// Deserialize directly into a config model:
///
/// ```rust,ignore
/// #[derive(serde::Deserialize, Default)]
/// struct MyConfig { /* … */ }
///
/// let config: MyConfig = loader.load().expect("deserialization should succeed");
/// ```
#[derive(Debug)]
pub struct ChainLoader {
    sources: Vec<Box<dyn ConfigSource>>,
}

impl ChainLoader {
    /// Create an empty chain loader (no sources).
    ///
    /// [`load_value`](Self::load_value) returns `Ok(None)` and
    /// [`load`](Self::load) returns the target type's [`Default`].
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Append a source to the end of the chain (highest priority so far).
    ///
    /// Returns `self` for chaining.
    pub fn add_source(mut self, source: Box<dyn ConfigSource>) -> Self {
        self.sources.push(source);
        self
    }

    /// Append a source to the end of the chain (mutable, non-consuming).
    pub fn push(&mut self, source: Box<dyn ConfigSource>) {
        self.sources.push(source);
    }

    /// Whether the chain has no sources configured.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Load and merge all sources in the chain.
    ///
    /// Later sources override earlier ones at the TOML table/key level. Returns
    /// `Ok(None)` when every source returned `None` (no content available).
    pub fn load_value(&self) -> Result<Option<toml::Value>, ConfigLoadError> {
        let mut merged: Option<toml::Value> = None;

        for source in &self.sources {
            let raw = match source.load_raw()? {
                Some(content) => content,
                None => continue,
            };

            let value: toml::Value = toml::from_str(&raw).map_err(|e| ConfigLoadError::Parse {
                source: source.description().to_string(),
                inner: e,
            })?;

            match &merged {
                None => merged = Some(value),
                Some(_) => merge_tables(merged.as_mut().unwrap(), value),
            }
        }

        Ok(merged)
    }

    /// Load, merge, and deserialize into the target config type.
    ///
    /// Returns [`T::default()`](Default::default) when every source returned `None`
    /// (no content available). Deserialization errors are surfaced as
    /// [`ConfigLoadError::Parse`].
    pub fn load<T: DeserializeOwned + Default>(&self) -> Result<T, ConfigLoadError> {
        match self.load_value()? {
            Some(value) => T::deserialize(value).map_err(|e| ConfigLoadError::Parse {
                source: "<merged>".to_string(),
                inner: e,
            }),
            None => Ok(T::default()),
        }
    }
}

impl Default for ChainLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Recursively merge `overlay` into `base` at the TOML value level.
///
/// - When both values are tables, the merge descends key-by-key.
/// - When either value is not a table (scalar, array, datetime), `overlay`
///   replaces `base`.
fn merge_tables(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_map), toml::Value::Table(overlay_map)) => {
            for (key, value) in overlay_map {
                if base_map.contains_key(&key) {
                    merge_tables(&mut base_map[&key], value);
                } else {
                    base_map.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FileSource;
    use serde::Deserialize;
    use std::io::Write;

    // ── helpers ────────────────────────────────────────────────────────────

    /// A trivial [`ConfigSource`] that returns a fixed string.
    #[derive(Debug)]
    struct InlineSource {
        content: Option<String>,
        label: String,
    }

    impl InlineSource {
        fn some(content: &str, label: &str) -> Self {
            Self {
                content: Some(content.to_string()),
                label: label.to_string(),
            }
        }
        fn none(label: &str) -> Self {
            Self {
                content: None,
                label: label.to_string(),
            }
        }
    }

    impl ConfigSource for InlineSource {
        fn load_raw(&self) -> Result<Option<String>, ConfigLoadError> {
            Ok(self.content.clone())
        }
        fn description(&self) -> &str {
            &self.label
        }
    }

    /// A test config model.
    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct TestConfig {
        name: String,
        count: Option<i64>,
    }

    /// A nested config model for merge testing.
    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct NestedConfig {
        outer: Option<Outer>,
    }

    #[derive(Debug, Default, Deserialize, PartialEq)]
    struct Outer {
        inner_key: Option<String>,
    }

    // ── empty / no-source tests ────────────────────────────────────────────

    #[test]
    fn new_loader_has_no_sources() {
        let loader = ChainLoader::new();
        assert!(loader.is_empty());
        assert!(loader.load_value().unwrap().is_none());
    }

    #[test]
    fn load_returns_default_when_no_sources_provide_content() {
        let loader = ChainLoader::new().add_source(Box::new(InlineSource::none("empty")));
        let config: TestConfig = loader.load().expect("should return default");
        assert_eq!(config, TestConfig::default());
    }

    // ── single source ──────────────────────────────────────────────────────

    #[test]
    fn single_source_returns_its_content() {
        let loader =
            ChainLoader::new().add_source(Box::new(InlineSource::some("name = \"test\"", "src1")));
        let value = loader.load_value().unwrap().expect("should have value");
        assert_eq!(value.get("name").and_then(|v| v.as_str()), Some("test"));
    }

    #[test]
    fn single_source_load_as_type() {
        let loader = ChainLoader::new().add_source(Box::new(InlineSource::some(
            "name = \"hello\"\ncount = 42",
            "src1",
        )));
        let config: TestConfig = loader.load().expect("deserialization should succeed");
        assert_eq!(config.name, "hello");
        assert_eq!(config.count, Some(42));
    }

    // ── multiple sources ───────────────────────────────────────────────────

    #[test]
    fn later_source_overrides_scalar() {
        let loader = ChainLoader::new()
            .add_source(Box::new(InlineSource::some("name = \"first\"", "low")))
            .add_source(Box::new(InlineSource::some("name = \"second\"", "high")));
        let config: TestConfig = loader.load().expect("should merge");
        assert_eq!(config.name, "second");
    }

    #[test]
    fn later_source_adds_keys() {
        let loader = ChainLoader::new()
            .add_source(Box::new(InlineSource::some("name = \"test\"", "low")))
            .add_source(Box::new(InlineSource::some("count = 99", "high")));
        let config: TestConfig = loader.load().expect("should merge");
        assert_eq!(config.name, "test");
        assert_eq!(config.count, Some(99));
    }

    #[test]
    fn tables_merge_recursively() {
        // low source provides outer.inner_key = "from_low"
        // high source provides outer.inner_key = "from_high"  (replaces)
        // high source does NOT override other keys at the top level
        let loader = ChainLoader::new()
            .add_source(Box::new(InlineSource::some(
                "[outer]\ninner_key = \"from_low\"\n",
                "low",
            )))
            .add_source(Box::new(InlineSource::some(
                "[outer]\ninner_key = \"from_high\"\n",
                "high",
            )));
        let config: NestedConfig = loader.load().expect("should merge tables");
        assert_eq!(
            config.outer.expect("outer should exist").inner_key,
            Some("from_high".to_string())
        );
    }

    #[test]
    fn absent_source_is_skipped() {
        let loader = ChainLoader::new()
            .add_source(Box::new(InlineSource::none("absent")))
            .add_source(Box::new(InlineSource::some(
                "name = \"present\"",
                "present",
            )));
        let config: TestConfig = loader.load().expect("should load from present source");
        assert_eq!(config.name, "present");
    }

    #[test]
    fn all_absent_returns_none_for_value() {
        let loader = ChainLoader::new()
            .add_source(Box::new(InlineSource::none("a")))
            .add_source(Box::new(InlineSource::none("b")));
        assert!(loader.load_value().unwrap().is_none());
    }

    // ── file-based chain ───────────────────────────────────────────────────

    #[test]
    fn chain_with_file_sources() {
        let dir = tempfile::TempDir::new().unwrap();

        // low priority file
        let low_path = dir.path().join("low.toml");
        let mut f = std::fs::File::create(&low_path).unwrap();
        f.write_all(b"name = \"from-low\"").unwrap();
        drop(f);

        // high priority file
        let high_path = dir.path().join("high.toml");
        let mut f = std::fs::File::create(&high_path).unwrap();
        f.write_all(b"count = 10").unwrap();
        drop(f);

        let loader = ChainLoader::new()
            .add_source(Box::new(FileSource::new(low_path)))
            .add_source(Box::new(FileSource::new(high_path)));

        let config: TestConfig = loader.load().expect("should merge file sources");
        assert_eq!(config.name, "from-low");
        assert_eq!(config.count, Some(10));
    }

    // ── error propagation ──────────────────────────────────────────────────

    #[test]
    fn parse_error_is_surfaced() {
        let loader = ChainLoader::new()
            .add_source(Box::new(InlineSource::some("not valid toml {{{", "bad")));
        let err = loader.load_value().unwrap_err();
        assert!(
            matches!(&err, ConfigLoadError::Parse { source, .. } if source == "bad"),
            "expected Parse error from 'bad', got: {err}"
        );
    }

    #[test]
    fn file_io_error_is_surfaced() {
        // A path that exists but is a directory — reading it yields an I/O error.
        let dir = tempfile::TempDir::new().unwrap();
        let dir_path = dir.path().to_path_buf();

        let loader = ChainLoader::new().add_source(Box::new(FileSource::new(dir_path)));
        let err = loader.load_value().unwrap_err();
        assert!(
            matches!(&err, ConfigLoadError::Io { .. }),
            "expected Io error, got: {err}"
        );
    }
}
