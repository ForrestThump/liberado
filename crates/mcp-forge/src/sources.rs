//! Loads `mcp-sources.toml` — the list of git-hosted MCP repos this crate builds and installs.
//! Kept separate from `topology.toml` (which stays human-owned for `description`/`consequence`,
//! see `liberado_config::McpConfig`'s doc comment); this file only ever answers "where
//! does the source live, and how do I build it."

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// One buildable MCP source. `name` is the join key with `topology.toml`'s `[[mcps]]` entry for
/// this MCP — it's both the install-directory key and the binary name
/// [`liberado_config::managed_binary_path`] resolves.
///
/// Exactly one of `git`/`path` must be set (validated in [`load_sources`], not by serde — TOML has
/// no clean tagged-union for "exactly one of these fields"). `git` builds via
/// `cargo install --git` in an isolated clone — the right choice for a genuinely standalone repo.
/// `path` builds via `cargo install --path` against a local directory instead — for co-developed
/// MCPs that depend on unpublished sibling crates (this workspace's own crates, or a fork like
/// `turbomcp`'s local checkout) and so can't be built from an isolated git clone at all
/// (`liberado-deliberate-mcp`, `riggers` are both this shape today).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpSource {
    pub name: String,
    #[serde(default)]
    pub git: Option<String>,
    /// A local directory (absolute, or relative to wherever `mcp-forge` is run from) built via
    /// `cargo install --path` instead of `--git`. Always rebuilt on `sync` — there's no remote SHA
    /// to check against, and re-running `cargo install` against unchanged local source is already
    /// cheap thanks to cargo's own incremental cache, so skip-if-unchanged isn't worth tracking
    /// here the way it is for a network fetch.
    #[serde(default)]
    pub path: Option<String>,
    /// Branch/tag/commit to install. Only meaningful for a `git` source; ignored for `path`.
    #[serde(default)]
    pub rev: Option<String>,
    /// Selects a package by name for a source that's a Cargo virtual workspace (multiple
    /// packages, no default) — e.g. `liberado-pdf-mcp`, whose real binary lives in package
    /// `mcp-pdf-server`, not a package named after the repo. Passed as `cargo install`'s trailing
    /// positional `CRATE` argument (`cargo install` has no `-p`/`--package` flag).
    #[serde(default)]
    pub package: Option<String>,
    /// `--bin` passthrough for a package that builds more than one binary.
    #[serde(default)]
    pub bin: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SourcesFile {
    #[serde(rename = "source", default)]
    source: Vec<McpSource>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourcesError {
    #[error("failed to read '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("source '{0}' declares neither git nor path — exactly one is required")]
    MissingLocation(String),
    #[error("source '{0}' declares both git and path — exactly one is required, not both")]
    AmbiguousLocation(String),
}

/// Load and parse `path` (typically `<config_dir>/mcp-sources.toml`) into its list of sources,
/// validating each declares exactly one of `git`/`path`. An absent file is a caller-level concern,
/// not handled here (unlike `topology.toml`'s optional-section convention — a missing sources file
/// means "nothing to sync", which the CLI reports directly rather than silently defaulting to an
/// empty list).
pub fn load_sources(path: &Path) -> Result<Vec<McpSource>, SourcesError> {
    let text = fs::read_to_string(path).map_err(|source| SourcesError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let file: SourcesFile = toml::from_str(&text).map_err(|source| SourcesError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    for source in &file.source {
        match (&source.git, &source.path) {
            (None, None) => return Err(SourcesError::MissingLocation(source.name.clone())),
            (Some(_), Some(_)) => return Err(SourcesError::AmbiguousLocation(source.name.clone())),
            _ => {}
        }
    }
    Ok(file.source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_and_full_sources() {
        let toml_text = r#"
[[source]]
name = "liberado-weather-mcp"
git = "https://github.com/ForrestThump/liberado-weather-mcp"

[[source]]
name = "liberado-pdf-mcp"
git = "https://github.com/ForrestThump/liberado-pdf-mcp"
rev = "main"
package = "mcp-pdf-server"
bin = "mcp-pdf-server"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-sources.toml");
        std::fs::write(&path, toml_text).unwrap();

        let sources = load_sources(&path).expect("valid sources file parses");
        assert_eq!(sources.len(), 2);

        assert_eq!(sources[0].name, "liberado-weather-mcp");
        assert_eq!(sources[0].rev, None);
        assert_eq!(sources[0].package, None);
        assert_eq!(sources[0].bin, None);

        assert_eq!(sources[1].name, "liberado-pdf-mcp");
        assert_eq!(sources[1].rev.as_deref(), Some("main"));
        assert_eq!(sources[1].package.as_deref(), Some("mcp-pdf-server"));
        assert_eq!(sources[1].bin.as_deref(), Some("mcp-pdf-server"));
    }

    #[test]
    fn parses_a_path_source() {
        let toml_text = r#"
[[source]]
name = "liberado-deliberate-mcp"
path = "../liberado-deliberate-mcp"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-sources.toml");
        std::fs::write(&path, toml_text).unwrap();

        let sources = load_sources(&path).expect("valid path source parses");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].git, None);
        assert_eq!(sources[0].path.as_deref(), Some("../liberado-deliberate-mcp"));
    }

    #[test]
    fn rejects_a_source_with_neither_git_nor_path() {
        let toml_text = r#"
[[source]]
name = "broken"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-sources.toml");
        std::fs::write(&path, toml_text).unwrap();

        let err = load_sources(&path).unwrap_err();
        assert!(matches!(err, SourcesError::MissingLocation(name) if name == "broken"));
    }

    #[test]
    fn rejects_a_source_with_both_git_and_path() {
        let toml_text = r#"
[[source]]
name = "broken"
git = "https://github.com/example/repo"
path = "../repo"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-sources.toml");
        std::fs::write(&path, toml_text).unwrap();

        let err = load_sources(&path).unwrap_err();
        assert!(matches!(err, SourcesError::AmbiguousLocation(name) if name == "broken"));
    }

    #[test]
    fn missing_file_is_a_read_error() {
        let err = load_sources(Path::new("/does/not/exist/mcp-sources.toml")).unwrap_err();
        assert!(matches!(err, SourcesError::Read { .. }));
    }

    #[test]
    fn invalid_toml_is_a_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-sources.toml");
        std::fs::write(&path, "not valid toml [[[").unwrap();
        let err = load_sources(&path).unwrap_err();
        assert!(matches!(err, SourcesError::Parse { .. }));
    }
}
