//! The config-file loader (Decision 14, `liberado-config-spec.md`).
//!
//! The typed *model* and its model-level [`Config::validate`] live in `liberado-common`; this is the
//! daemon-side half: resolve a config directory, read the three optional per-section TOML files
//! (`topology.toml` / `policy.toml` / `tuning.toml`), assemble them into one [`Config`], and run the
//! **cross-cutting** checks that need more than one section to verify (dangling zone/MCP refs,
//! missing secrets). Every error names the offending file or setting, because the realistic edit path
//! for this config is an `ssh` session — the message has to be enough to fix it without a debugger.
//!
//! Each file is optional: an absent file leaves its section at the specced `Default` (so an empty
//! config still assembles a `Config`, which then fails validation citing e.g. the missing vault path
//! — a precise, actionable failure, not a silent one).

use std::path::{Path, PathBuf};

use liberado_common::Capability;
use liberado_common::config::Config;
use thiserror::Error;

/// Where the loader looks for config: `LIBERADO_CONFIG_DIR/liberado` is not assumed — the env var
/// names the directory directly; otherwise the platform config dir gets a `liberado` subfolder.
const CONFIG_DIR_ENV: &str = "LIBERADO_CONFIG_DIR";
const APP_DIR: &str = "liberado";

/// The three section files the loader reads, paired with how each maps onto a [`Config`] section.
const TOPOLOGY_FILE: &str = "topology.toml";
const POLICY_FILE: &str = "policy.toml";
const TUNING_FILE: &str = "tuning.toml";

/// A config load/validation failure. Each variant carries the file (or section) at fault so the
/// message is actionable after a remote edit.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A present file could not be read (permissions, mid-write, etc.). An *absent* file is not an
    /// error — it falls back to that section's `Default`.
    #[error("failed to read config file '{file}': {source}")]
    Read {
        file: String,
        #[source]
        source: std::io::Error,
    },

    /// A file was read but is not valid TOML for its section type. The toml error already points at
    /// the line/column; we prefix it with the file so the user knows which one to open.
    #[error("failed to parse config file '{file}': {source}")]
    Parse {
        file: String,
        #[source]
        source: toml::de::Error,
    },

    /// The assembled config failed validation (model-level invariants or the cross-cutting checks
    /// below). The string is already prefixed with the section/setting at fault.
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Resolve the config directory: `LIBERADO_CONFIG_DIR` if set, else the platform config dir
/// (`dirs::config_dir()/liberado`). Returns `None` if neither is available (a headless environment
/// with no env var and no home), in which case the caller boots on all-defaults.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }
    dirs::config_dir().map(|base| base.join(APP_DIR))
}

/// Load `topology.toml`, `policy.toml`, `tuning.toml` from `dir` (each OPTIONAL — an absent file
/// leaves that section at its `Default`), assemble a [`Config`], and validate it. Returns the
/// validated config or an actionable error. `dir = None` => an all-defaults `Config` (still
/// validated, so e.g. a missing `vault_path` is reported rather than silently accepted).
pub fn load_config(dir: Option<&Path>) -> Result<Config, ConfigError> {
    let topology = load_section(dir, TOPOLOGY_FILE)?;
    let policy = load_section(dir, POLICY_FILE)?;
    let tuning = load_section(dir, TUNING_FILE)?;

    let config = Config {
        topology,
        policy,
        tuning,
    };

    // Model-level invariants first (vault path, role floors, …), then the cross-cutting checks that
    // need more than one section to verify. `Error::Config` already Displays as "invalid config: …",
    // and so does our `Invalid` — strip the duplicate prefix so the message isn't doubled.
    config.validate().map_err(|e| {
        let msg = e.to_string();
        ConfigError::Invalid(
            msg.strip_prefix("invalid config: ")
                .unwrap_or(&msg)
                .to_string(),
        )
    })?;
    validate_cross_cutting(&config)?;

    Ok(config)
}

/// Read one section file into its type. A missing file yields the type's `Default` (every section is
/// `#[serde(default)]`, so an empty `""` deserialises to the same all-defaults value — we short-
/// circuit it for clarity). A present-but-unreadable file is a [`ConfigError::Read`]; a present file
/// with bad TOML is a [`ConfigError::Parse`], both naming the file.
fn load_section<T>(dir: Option<&Path>, file: &str) -> Result<T, ConfigError>
where
    T: serde::de::DeserializeOwned + Default,
{
    let Some(dir) = dir else {
        return Ok(T::default());
    };
    let path = dir.join(file);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        // Absent file => that section's Default. Any other I/O error is a real failure.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(source) => {
            return Err(ConfigError::Read {
                file: file.to_string(),
                source,
            });
        }
    };
    toml::from_str(&contents).map_err(|source| ConfigError::Parse {
        file: file.to_string(),
        source,
    })
}

/// The cross-cutting validation layered on top of [`Config::validate`]: checks that span sections
/// and so can only be verified once everything is assembled. Returns the first failure, each message
/// naming the offending entry.
///
/// 1. Every zone named in a grant capability must be declared in `policy.zones`.
/// 2. Every `ExecuteMcp(name)` in a grant must name an MCP present in `topology.mcps`.
/// 3. Every `policy.secret_refs` entry must be set as an environment variable (value never printed).
fn validate_cross_cutting(config: &Config) -> Result<(), ConfigError> {
    let invalid = |msg: String| ConfigError::Invalid(msg);

    for grant in &config.policy.grants {
        for cap in &grant.capabilities {
            match cap {
                Capability::Read(zone)
                | Capability::Write(zone)
                | Capability::ReadSummary(zone) => {
                    let name = zone_name(zone);
                    if !config.policy.zones.iter().any(|z| z.zone == name) {
                        return Err(invalid(format!(
                            "grant references undeclared zone '{name}'"
                        )));
                    }
                }
                Capability::ExecuteMcp(mcp) => {
                    if !config.topology.mcps.iter().any(|c| &c.name == mcp) {
                        return Err(invalid(format!(
                            "grant references unknown MCP '{mcp}' (not in topology.mcps)"
                        )));
                    }
                }
            }
        }
    }

    for secret in &config.policy.secret_refs {
        if std::env::var_os(secret).is_none() {
            return Err(invalid(format!(
                "secret_ref '{secret}' has no corresponding environment variable"
            )));
        }
    }

    Ok(())
}

/// The bare name a zone capability references, regardless of vault/named kind — both are matched
/// against `policy.zones` by name (zones are declared by name, not by kind).
fn zone_name(zone: &liberado_common::Zone) -> &str {
    match zone {
        liberado_common::Zone::Vault(name) | liberado_common::Zone::Named(name) => name,
    }
}

/// Build the dispatcher's catalog from the ENABLED MCPs in `config.topology.mcps`. This is what the
/// dispatcher routes over (and the consequence guard gates on); an empty catalog means the dispatcher
/// can route to nothing (the pre-slice-2 state).
pub fn catalog_from_config(config: &Config) -> Vec<liberado_dispatcher::McpDescriptor> {
    config
        .topology
        .mcps
        .iter()
        .filter(|m| m.enabled)
        .map(|m| liberado_dispatcher::McpDescriptor {
            name: m.name.clone(),
            description: m.description.clone(),
            consequence: m.consequence,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::WriteClass;
    use liberado_common::capability::Consequence;
    use liberado_common::config::{McpConfig, McpTransport};
    use std::io::Write;
    use tempfile::TempDir;

    /// Write `contents` to `dir/name`.
    fn write_file(dir: &Path, name: &str, contents: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    /// A topology naming a vault path and a single MCP (`memory-mcp`). Description + consequence are
    /// required on every `[[mcps]]` entry (the fail-fast contract).
    const TOPOLOGY_TOML: &str = r#"
vault_path = "/home/shiloh/vault"

[[mcps]]
name = "memory-mcp"
description = "store and recall memories"
consequence = "reversible"
transport = { kind = "stdio", command = "memory-mcp", args = [] }
"#;

    /// A policy with two writable zones, a human-only zone, a grant, and no secrets.
    const POLICY_TOML: &str = r#"
[[zones]]
zone = "tasks"
write_class = "agent_writable"

[[zones]]
zone = "decisions"
write_class = "agent_writable"

[[zones]]
zone = "finance"
write_class = "human_only"

[[grants]]
component = "agent"
capabilities = [
    { Read = { Vault = "tasks" } },
    { Write = { Vault = "tasks" } },
    { Read = { Vault = "decisions" } },
    { ExecuteMcp = "memory-mcp" },
]
"#;

    #[test]
    fn loads_and_parses_policy_and_topology() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "topology.toml", TOPOLOGY_TOML);
        write_file(dir.path(), "policy.toml", POLICY_TOML);

        let config = load_config(Some(dir.path())).expect("valid config should load");

        assert_eq!(config.policy.zones.len(), 3);
        assert_eq!(config.policy.grants.len(), 1);
        assert_eq!(config.topology.mcps.len(), 1);

        // write_class lookups: declared zones resolve, an unlisted zone fails safe to ProposalOnly.
        assert_eq!(
            config.policy.write_class("tasks"),
            WriteClass::AgentWritable
        );
        assert_eq!(config.policy.write_class("finance"), WriteClass::HumanOnly);
        assert_eq!(
            config.policy.write_class("does-not-exist"),
            WriteClass::ProposalOnly
        );

        // base_capabilities is the union of the single grant's caps.
        let caps = config.policy.base_capabilities();
        assert!(caps.contains(&Capability::Read(liberado_common::Zone::vault("tasks"))));
        assert!(caps.contains(&Capability::Write(liberado_common::Zone::vault("tasks"))));
        assert!(caps.contains(&Capability::Read(liberado_common::Zone::vault("decisions"))));
        assert!(caps.contains(&Capability::ExecuteMcp("memory-mcp".into())));
        assert_eq!(caps.capabilities.len(), 4);
    }

    #[test]
    fn rejects_grant_referencing_undeclared_zone() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "topology.toml", TOPOLOGY_TOML);
        // `decisions` is granted but never declared in zones.
        write_file(
            dir.path(),
            "policy.toml",
            r#"
[[zones]]
zone = "tasks"
write_class = "agent_writable"

[[grants]]
component = "agent"
capabilities = [ { Write = { Vault = "decisions" } } ]
"#,
        );

        let err = load_config(Some(dir.path())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("undeclared zone"), "got: {msg}");
        assert!(msg.contains("decisions"), "should name the zone: {msg}");
    }

    #[test]
    fn rejects_grant_referencing_unknown_mcp() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "topology.toml", TOPOLOGY_TOML);
        write_file(
            dir.path(),
            "policy.toml",
            r#"
[[grants]]
component = "agent"
capabilities = [ { ExecuteMcp = "ghost-mcp" } ]
"#,
        );

        let err = load_config(Some(dir.path())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown MCP"), "got: {msg}");
        assert!(msg.contains("ghost-mcp"), "should name the MCP: {msg}");
    }

    #[test]
    fn rejects_secret_ref_with_no_env_var() {
        let dir = TempDir::new().unwrap();
        write_file(dir.path(), "topology.toml", TOPOLOGY_TOML);
        // A secret name that is overwhelmingly unlikely to exist in the environment.
        write_file(
            dir.path(),
            "policy.toml",
            r#"secret_refs = ["LIBERADO_TEST_DEFINITELY_UNSET_SECRET_XYZZY"]"#,
        );

        let err = load_config(Some(dir.path())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("secret_ref"), "got: {msg}");
        assert!(
            msg.contains("LIBERADO_TEST_DEFINITELY_UNSET_SECRET_XYZZY"),
            "should name the secret: {msg}"
        );
    }

    #[test]
    fn absent_files_default_then_fail_on_missing_vault_path() {
        // An empty dir => all-defaults Config; validation must fail citing the vault path.
        let dir = TempDir::new().unwrap();
        let err = load_config(Some(dir.path())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("vault_path"), "should cite vault_path: {msg}");
    }

    #[test]
    fn dir_none_is_all_defaults_and_fails_validation() {
        // No directory at all behaves like an empty one: defaults, then the vault-path check fires.
        let err = load_config(None).unwrap_err();
        assert!(err.to_string().contains("vault_path"));
    }

    #[test]
    fn catalog_only_includes_enabled_mcps_carrying_metadata() {
        let mut config = Config::default();
        config.topology.mcps = vec![
            McpConfig {
                name: "tasks-mcp".into(),
                enabled: true,
                description: "create and complete tasks".into(),
                consequence: Consequence::Reversible,
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@scope/tasks".into()],
                },
            },
            McpConfig {
                name: "email-mcp".into(),
                enabled: false, // disabled => must NOT appear in the catalog
                description: "send email".into(),
                consequence: Consequence::External,
                transport: McpTransport::Stdio {
                    command: "email-mcp".into(),
                    args: vec![],
                },
            },
        ];

        let catalog = catalog_from_config(&config);
        assert_eq!(catalog.len(), 1, "only the enabled MCP routes");
        let entry = &catalog[0];
        assert_eq!(entry.name, "tasks-mcp");
        assert_eq!(entry.description, "create and complete tasks");
        assert_eq!(entry.consequence, Consequence::Reversible);
    }

    #[test]
    fn loads_mcp_with_description_and_consequence() {
        let dir = TempDir::new().unwrap();
        // `external` must deserialize (snake_case) into Consequence::External.
        write_file(
            dir.path(),
            "topology.toml",
            r#"
vault_path = "/home/shiloh/vault"

[[mcps]]
name = "email-mcp"
description = "send email on the user's behalf"
consequence = "external"
transport = { kind = "stdio", command = "email-mcp", args = [] }
"#,
        );

        let config = load_config(Some(dir.path())).expect("valid config should load");
        assert_eq!(config.topology.mcps.len(), 1);
        let mcp = &config.topology.mcps[0];
        assert_eq!(mcp.name, "email-mcp");
        assert_eq!(mcp.description, "send email on the user's behalf");
        assert_eq!(mcp.consequence, Consequence::External);
        assert!(mcp.enabled, "enabled defaults to true");
    }

    #[test]
    fn rejects_mcp_missing_consequence() {
        let dir = TempDir::new().unwrap();
        // No `consequence` field: there is no serde default, so the parse must fail (fail-fast).
        write_file(
            dir.path(),
            "topology.toml",
            r#"
vault_path = "/home/shiloh/vault"

[[mcps]]
name = "tasks-mcp"
description = "create and complete tasks"
transport = { kind = "stdio", command = "tasks-mcp", args = [] }
"#,
        );

        let err = load_config(Some(dir.path())).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("topology.toml"),
            "error should name the file: {msg}"
        );
        assert!(
            msg.contains("consequence"),
            "error should reference the missing field: {msg}"
        );
    }
}
