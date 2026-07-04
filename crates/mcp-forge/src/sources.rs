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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct McpSource {
    pub name: String,
    pub git: String,
    /// Branch/tag/commit to install. Omitted = track the remote default branch.
    #[serde(default)]
    pub rev: Option<String>,
    /// Selects a package by name for git sources that are Cargo virtual workspaces (multiple
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
}

/// Load and parse `path` (typically `<config_dir>/mcp-sources.toml`) into its list of sources.
/// An absent file is a caller-level concern, not handled here (unlike `topology.toml`'s
/// optional-section convention — a missing sources file means "nothing to sync", which the CLI
/// reports directly rather than silently defaulting to an empty list).
pub fn load_sources(path: &Path) -> Result<Vec<McpSource>, SourcesError> {
    let text = fs::read_to_string(path).map_err(|source| SourcesError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let file: SourcesFile = toml::from_str(&text).map_err(|source| SourcesError::Parse {
        path: path.display().to_string(),
        source,
    })?;
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
