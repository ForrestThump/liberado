//! # liberado-config
//!
//! The config-file loader (Decision 14, `liberado-config-spec.md`), plus the path-resolution helpers
//! built directly on it. This is the daemon-side half: resolve a config directory, read the three
//! optional per-section TOML files (`topology.toml` / `policy.toml` / `tuning.toml`), assemble them
//! into one [`Config`], and run the **cross-cutting** checks (dangling zone/MCP refs, missing secrets)
//! via the config-loader crate. Every error names the offending file or setting, because the realistic
//! edit path for this config is an `ssh` session — the message has to be enough to fix it without a
//! debugger.
//!
//! Each file is optional: an absent file leaves its section at the specced `Default` (so an empty
//! config still assembles a `Config`, which then fails validation citing e.g. the missing vault path
//! — a precise, actionable failure, not a silent one).
//!
//! The typed *model* (`Config`/`Topology`/`Policy`/`Tuning`/…) and its model-level
//! [`Config::validate`] live in `liberado-config-loader`, not here — that crate's own cross-cutting
//! validation needs the model, and this crate already depends on it, so putting the model here
//! instead would create a cycle (moved from `liberado-common` 2026-07-04,
//! `docs/roadmap/hygiene-audit-2026-07-04.md`). Re-exported below so callers still reach it as
//! `liberado_config::Config` et al. — "the config crate" stays the natural place to import it from.
//!
//! Deliberately dependency-light: only `liberado-common` + `liberado-config-loader`, no
//! daemon/mcp/dispatcher/orchestrator/provider — so a tool that only needs config/path resolution
//! (`liberado-mcp-forge`) doesn't have to build the whole assembly stack. `liberado-bootstrap` depends
//! on this crate and re-exports its public surface, so `liberado-server`/`liberado-cli` see no change.

use std::path::{Path, PathBuf};

use liberado_common::CapabilityCatalog;
use thiserror::Error;

pub use liberado_config_loader::{
    CURRENT_SCHEMA_VERSION, CaptureTuning, ComponentConfig, ConcurrencyTuning, Config, ConfigBuilder,
    ContextTuning, DispatchTuning, Grant, MaintenanceTuning, McpConfig, McpTransport, Policy,
    SubagentIsolation, Topology, Tuning, ZonePolicy, managed_binary_path,
};

/// Records which source file contributed each section of a loaded [`Config`],
/// or `None` if that section fell back to its built-in [`Default`].
///
/// Returned alongside [`Config`] by [`load_config`] so callers can report
/// per-value provenance in diagnostics (Decision 14; see `docs/specs/liberado-config-spec.md`).
#[derive(Debug, Clone)]
pub struct ConfigProvenance {
    /// The source file for `[topology]` values, or `None` if the file was absent.
    pub topology: Option<String>,
    /// The source file for `[policy]` values, or `None` if the file was absent.
    pub policy: Option<String>,
    /// The source file for `[tuning]` values, or `None` if the file was absent.
    pub tuning: Option<String>,
}

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

/// Resolve the config directory:
///
/// 1. `LIBERADO_CONFIG_DIR` env var — explicit intent, always wins.
/// 2. Platform config dir (`dirs::config_dir()/liberado`), but only if it already contains at
///    least one config section file.
/// 3. Development convenience: walk up from the running binary, checking each ancestor for a
///    `config/` subdirectory that has at least one config section file. This covers the common
///    layout where `liberado.exe` lives in `target/release/` and config files sit in the
///    project-root `config/` directory.
/// 4. Platform config dir as final fallback (even if empty), so a fresh install gets a clear
///    "vault_path is required" error rather than silent defaults.
///
/// Returns `None` if none of the above yields a directory (headless environment, no env var,
/// no home, and no binary path).
pub fn config_dir() -> Option<PathBuf> {
    // 1. Explicit env var always wins.
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }

    // 2. Platform config dir, but only if it already has config files.
    let platform = dirs::config_dir().map(|base| base.join(APP_DIR));
    if let Some(ref dir) = platform {
        if has_any_config_file(dir) {
            return Some(dir.clone());
        }
    }

    // 3. Walk up from the binary checking for a `config/` subdirectory.
    if let Ok(exe) = std::env::current_exe() {
        let mut current = exe.parent().map(Path::to_path_buf);
        for _ in 0..5 {
            if let Some(ref dir) = current {
                let candidate = dir.join("config");
                if has_any_config_file(&candidate) {
                    return Some(candidate);
                }
                current = dir.parent().map(Path::to_path_buf);
            } else {
                break;
            }
        }
    }

    // 4. Final fallback: platform config dir (may be empty — validated downstream).
    platform
}

/// True when `dir` exists and contains at least one of the three known config section files.
fn has_any_config_file(dir: &Path) -> bool {
    dir.join(TOPOLOGY_FILE).exists()
        || dir.join(POLICY_FILE).exists()
        || dir.join(TUNING_FILE).exists()
}

/// Load `topology.toml`, `policy.toml`, `tuning.toml` from `dir` (each OPTIONAL — an absent file
/// leaves that section at its `Default`), assemble a [`Config`], validate it, and return the
/// validated config alongside a [`ConfigProvenance`] that records which source file contributed
/// each section. `dir = None` => an all-defaults `Config` (still validated, so e.g. a missing
/// `vault_path` is reported rather than silently accepted).
pub fn load_config(dir: Option<&Path>) -> Result<(Config, ConfigProvenance), ConfigError> {
    let provenance = ConfigProvenance {
        topology: dir
            .filter(|d| d.join(TOPOLOGY_FILE).exists())
            .map(|_| TOPOLOGY_FILE.to_string()),
        policy: dir
            .filter(|d| d.join(POLICY_FILE).exists())
            .map(|_| POLICY_FILE.to_string()),
        tuning: dir
            .filter(|d| d.join(TUNING_FILE).exists())
            .map(|_| TUNING_FILE.to_string()),
    };

    let topology = load_section(dir, TOPOLOGY_FILE)?;
    let policy = load_section(dir, POLICY_FILE)?;
    let tuning: Tuning = load_section(dir, TUNING_FILE)?;

    // Warn if the tuning file carries a schema_version that differs from the current one.
    // This is a soft deprecation signal: users who copied an old `tuning.toml` and never
    // updated it will see a warning, but the config still loads (all fields default).
    if let Some(ref ver) = tuning.schema_version {
        if ver != CURRENT_SCHEMA_VERSION {
            tracing::warn!(
                "tuning.toml schema_version '{}' does not match current '{}' \
                 — the file may be outdated; consider reviewing config.example/tuning.toml",
                ver,
                CURRENT_SCHEMA_VERSION,
            );
        }
    }

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

    // Cross-cutting validation (dangling zone/MCP refs, missing secrets) moved into the
    // config-loader crate. Its `Validation` variant carries a bare message (no "invalid config:"
    // prefix) so wrapping in `ConfigError::Invalid` yields the same output as before.
    liberado_config_loader::validate_merged_config(&config)
        .map_err(|e| ConfigError::Invalid(e.to_string()))?;

    Ok((config, provenance))
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

/// Build the dispatcher's catalog from the ENABLED MCPs in `config.topology.mcps`. This is what the
/// dispatcher routes over (and the consequence guard gates on); an empty catalog means the dispatcher
/// can route to nothing (the pre-slice-2 state). Returns `liberado_common::McpDescriptor` — the same
/// type backing the live `CapabilityCatalog`, so this is also the snapshot function
/// `CapabilityCatalog` is seeded from at boot (see `crates/server/src/lib.rs::run`).
pub fn catalog_from_config(config: &Config) -> Vec<liberado_common::McpDescriptor> {
    config
        .topology
        .mcps
        .iter()
        .filter(|m| m.enabled)
        .map(|m| liberado_common::McpDescriptor {
            name: m.name.clone(),
            description: m.description.clone(),
            consequence: m.consequence,
            provenance: None,
        })
        .collect()
}

/// Build a live [`CapabilityCatalog`] from `config.topology.mcps` — the same descriptors
/// [`catalog_from_config`] produces, pre-registered into the shared, queryable object every
/// consumer (the server's `/api/catalog`, the daemon's reactive dispatch, chat's own dispatch)
/// reads from. Callers wrap the result in `Arc` and share ONE instance (see
/// `crates/server/src/lib.rs::run`) rather than each building their own snapshot.
pub fn capability_catalog_from_config(config: &Config) -> CapabilityCatalog {
    let catalog = CapabilityCatalog::new();
    for descriptor in catalog_from_config(config) {
        catalog.register(descriptor);
    }
    catalog
}

/// Where `liberado-mcp-forge` installs managed MCP binaries, and where
/// [`McpTransport::Managed`] resolution looks for
/// them (Decision: convention over mutation — `topology.toml` never gets a file path written into
/// it; a `name` resolves to a path by this one rule, on both the forge tool's and the daemon's side).
///
/// 1. `LIBERADO_MCP_INSTALL_DIR` env var — explicit intent, always wins.
/// 2. Platform data dir (`dirs::data_dir()/liberado/mcp-bin`), mirroring how [`config_dir`]
///    resolves `LIBERADO_CONFIG_DIR`.
pub fn mcp_install_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LIBERADO_MCP_INSTALL_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("liberado")
        .join("mcp-bin")
}

/// Where operational data (conversation logs, proposal files) lives — outside the vault (Decision 12),
/// so a vault watcher never reacts to it. `LIBERADO_DATA_DIR` env var wins; otherwise `.liberado`
/// relative to the working directory. Shared by both the chat-boot path (`liberado-server`) and the
/// daemon-boot path (`liberado-bootstrap`), which each used to resolve this independently.
pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into()))
}

/// The ingredients for `RiskGatedToolRuntime`-style guarding — chat's own runtime gate and the
/// `Orchestrator`'s runtime-level gate for adaptive tool calls both need the same consequence
/// catalog, the same proposals directory, and the same proposal-integrity signer. Shared by both
/// boot paths (`liberado-server`'s `build_chat`, `liberado-bootstrap`'s `configure_daemon`), which
/// each used to derive these independently.
///
/// `proposals_dir` is the **vault's** `proposals/` directory (not a data-dir path) — a runtime-level
/// downgrade needs to land where the daemon's `react()` actually watches, so approving one has
/// somewhere to go. See `RiskGatedToolRuntime`'s doc comment for why this used to be data-dir-only
/// and why that was a dead end.
pub struct GuardContext {
    pub consequences: Vec<(String, liberado_common::Consequence)>,
    pub proposals_dir: PathBuf,
    pub signer: liberado_common::ProposalSigner,
}

/// Build a [`GuardContext`] from the live capability catalog and the vault path a runtime-level
/// proposal downgrade should be written under.
pub fn guard_context(catalog: &CapabilityCatalog, vault_path: &Path) -> GuardContext {
    GuardContext {
        consequences: catalog.consequence_catalog(),
        proposals_dir: vault_path.to_path_buf(),
        signer: liberado_common::ProposalSigner::new(load_or_create_proposal_key()),
    }
}

const PROPOSAL_KEY_FILE: &str = ".proposal-key";
const PROPOSAL_KEY_LEN: usize = 32;

/// Load the per-installation proposal-signing key from `<data_dir>/.proposal-key`, generating and
/// persisting a fresh random one on first use. If persisting fails (e.g. a permissions error), logs
/// a warning and falls back to an ephemeral in-memory key for this run — proposals signed this run
/// then simply won't verify after a restart, which is a safe failure mode (rejected, not silently
/// accepted), not a dangerous one. See [`liberado_common::Proposal::integrity`] for what this key
/// does and doesn't defend against.
pub fn load_or_create_proposal_key() -> Vec<u8> {
    let path = data_dir().join(PROPOSAL_KEY_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == PROPOSAL_KEY_LEN {
            return bytes;
        }
    }
    let mut key = vec![0u8; PROPOSAL_KEY_LEN];
    {
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut key);
    }
    if let Err(e) = persist_proposal_key(&path, &key) {
        tracing::warn!(
            error = %e,
            "failed to persist the proposal signing key — using an ephemeral one for this run; \
             proposals created now won't verify after a restart"
        );
    }
    key
}

fn persist_proposal_key(path: &Path, key: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::Capability;
    use liberado_common::WriteClass;
    use liberado_common::capability::Consequence;
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

        let (config, prov) = load_config(Some(dir.path())).expect("valid config should load");

        assert_eq!(config.policy.zones.len(), 3);
        assert_eq!(config.policy.grants.len(), 1);
        assert_eq!(config.topology.mcps.len(), 1);

        // Provenance: both files were present.
        assert_eq!(prov.topology.as_deref(), Some("topology.toml"));
        assert_eq!(prov.policy.as_deref(), Some("policy.toml"));
        assert!(prov.tuning.is_none(), "no tuning.toml was written");

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

        // capabilities_for("agent") is the union of the single grant's caps (the fixture's
        // component is "agent" — see POLICY_TOML above).
        let caps = config.policy.capabilities_for("agent");
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

        let (config, prov) = load_config(Some(dir.path())).expect("valid config should load");
        assert_eq!(config.topology.mcps.len(), 1);
        let mcp = &config.topology.mcps[0];
        assert_eq!(mcp.name, "email-mcp");
        assert_eq!(mcp.description, "send email on the user's behalf");
        assert_eq!(mcp.consequence, Consequence::External);
        assert!(mcp.enabled, "enabled defaults to true");

        // Only topology.toml was present.
        assert_eq!(prov.topology.as_deref(), Some("topology.toml"));
        assert!(prov.policy.is_none());
        assert!(prov.tuning.is_none());
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
