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

use liberado_common::{Capability, CapabilityCatalog, WriteClass, Zone};
use thiserror::Error;

pub use liberado_config_loader::{
    COMPACTION_TRIGGER_PCT_DEFAULT, COMPACTION_TRIGGER_TOKENS_FALLBACK, CURRENT_SCHEMA_VERSION,
    CaptureTuning, CompactionSettings, ConcurrencyTuning, Config, ConfigBuilder, ContextTuning,
    CronSchedule, DEFAULT_POOL, DispatchTuning, Grant, HookConfig, MainAgentConfig,
    MaintenanceTuning, McpConfig, McpPoolingTuning, McpTransport, ModelCompactionSettings, Policy,
    PoolConfig, ProviderProfile, RoleOverride, SubagentIsolation, TelegramApprovalsTuning,
    ToolImpact, Topology, Tuning, ZonePolicy, managed_binary_path, resolve_declared_zone,
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
    let env_dir = std::env::var_os(CONFIG_DIR_ENV).map(PathBuf::from);
    let platform_dir = dirs::config_dir().map(|base| base.join(APP_DIR));
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    resolve_config_dir(env_dir, platform_dir, exe_dir)
}

/// The pure tiered-resolution logic `config_dir()` wraps, with its three real-world inputs
/// (env var, platform dir, running-binary's directory) taken as plain parameters instead of read
/// directly — so the 4-tier fallback order is unit-testable without mutating process-global env
/// vars (`dirs::config_dir()` reads platform env vars like `APPDATA`/`XDG_CONFIG_HOME` that aren't
/// mockable, and process env mutation races under `cargo test`'s default parallel execution).
fn resolve_config_dir(
    env_dir: Option<PathBuf>,
    platform_dir: Option<PathBuf>,
    exe_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    // 1. Explicit env var always wins.
    if let Some(dir) = env_dir
        && !dir.as_os_str().is_empty()
    {
        return Some(dir);
    }

    // 2. Platform config dir, but only if it already has config files.
    if let Some(ref dir) = platform_dir
        && has_any_config_file(dir)
    {
        return Some(dir.clone());
    }

    // 3. Walk up from the binary checking for a `config/` subdirectory.
    let mut current = exe_dir;
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

    // 4. Final fallback: platform config dir (may be empty — validated downstream).
    platform_dir
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
    if let Some(ref ver) = tuning.schema_version
        && ver != CURRENT_SCHEMA_VERSION
    {
        tracing::warn!(
            "tuning.toml schema_version '{}' does not match current '{}' \
                 — the file may be outdated; consider reviewing config.example/tuning.toml",
            ver,
            CURRENT_SCHEMA_VERSION,
        );
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
    // prefix) so wrapping in `ConfigError::Invalid` yields the same output as before. This validates
    // the *base* (hand-edited) config — a broken `policy.toml` stays fail-fast.
    liberado_config_loader::validate_merged_config(&config)
        .map_err(|e| ConfigError::Invalid(e.to_string()))?;

    // Machine-owned grants overlay (Telegram "Approve everywhere" — see `grants_overlay_path`).
    // Merged on top of the validated base by APPENDING its grants/zones, never rewriting the
    // hand-edited `policy.toml`. Deliberately **soft**: unlike the base, a broken or invalidating
    // overlay must never brick boot — worst case we ignore it and grant nothing extra. So if the
    // merged candidate fails the same cross-cutting validation, we discard the overlay and boot on
    // `policy.toml` alone (with a loud warning), rather than refusing to start.
    let overlay = load_grants_overlay();
    if overlay.grants.is_empty() && overlay.zones.is_empty() {
        return Ok((config, provenance));
    }
    let mut candidate = config.clone();
    merge_overlay_into(&mut candidate.policy, overlay);
    match liberado_config_loader::validate_merged_config(&candidate) {
        Ok(()) => {
            tracing::info!(
                grants = candidate.policy.grants.len(),
                zones = candidate.policy.zones.len(),
                "applied machine-owned grants overlay ({})",
                grants_overlay_path().display(),
            );
            Ok((candidate, provenance))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "grants overlay produced an invalid merged config — ignoring it and booting on \
                 policy.toml alone; the offending overlay is {}",
                grants_overlay_path().display(),
            );
            Ok((config, provenance))
        }
    }
}

/// The machine-owned grants-overlay filename. It lives in the **writable data dir** (not the config
/// dir — which is a read-only mount in the homelab deploy), and outside every vault zone, so no
/// agent capability can write it: the "agents can't edit their own permission config" invariant
/// holds. Only the daemon, reacting to a human Telegram button tap, ever writes here (see
/// [`append_grant_to_overlay`]).
const GRANTS_OVERLAY_FILE: &str = "grants.overlay.toml";

/// A one-line header stamped atop the generated overlay so a human who opens it knows it's
/// machine-owned and how it got there.
const GRANTS_OVERLAY_HEADER: &str = "machine-owned — appended by Liberado when you tap \"Approve everywhere\" on a permission \
     request. Merged over policy.toml at boot. Safe to delete to revoke all such grants.";

/// Path to the machine-owned grants overlay ([`GRANTS_OVERLAY_FILE`], under [`data_dir`]).
pub fn grants_overlay_path() -> PathBuf {
    data_dir().join(GRANTS_OVERLAY_FILE)
}

/// Read the machine-owned grants overlay ([`grants_overlay_path`]) as a partial [`Policy`] (only its
/// `zones`/`grants` are meaningful). An absent file is the common case → an empty overlay. A
/// present-but-unreadable/unparseable overlay is a **soft** failure: logged and treated as empty,
/// never fatal — fail-safe means "grant nothing extra when in doubt."
pub fn load_grants_overlay() -> Policy {
    load_grants_overlay_at(&grants_overlay_path())
}

/// [`load_grants_overlay`] against an explicit path — the testable core (the public wrapper resolves
/// the path from the process-global data dir, which can't be mocked without racing other tests).
fn load_grants_overlay_at(path: &Path) -> Policy {
    match std::fs::read_to_string(path) {
        Ok(contents) => match toml::from_str::<Policy>(&contents) {
            Ok(policy) => policy,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(), error = %e,
                    "grants overlay did not parse — ignoring it (no extra grants applied)"
                );
                Policy::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Policy::default(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(), error = %e,
                "grants overlay could not be read — ignoring it (no extra grants applied)"
            );
            Policy::default()
        }
    }
}

/// Append the overlay's zones and grants onto `policy` — never replace. Base entries keep priority:
/// [`Policy::write_class`] and [`Policy::capabilities_for`] scan in order and the base comes first,
/// so an overlay can only ADD authority for a genuinely-undeclared zone; it can never downgrade a
/// base zone's write-class (e.g. re-open a `human_only` zone) or shadow a base grant. `secret_refs`
/// is deliberately not merged — the overlay is grants-only.
fn merge_overlay_into(policy: &mut Policy, overlay: Policy) {
    policy.zones.extend(overlay.zones);
    policy.grants.extend(overlay.grants);
}

/// Persist a human-approved **"everywhere"** grant to the machine-owned overlay
/// ([`grants_overlay_path`]): append `capability` to `component`'s grant (creating the grant if
/// absent). So the merged config stays valid ([`validate_merged_config`] requires every granted zone
/// be declared in `policy.zones`), it also ensures the overlay declares any vault/named zone the
/// capability references, as [`WriteClass::AgentWritable`]. Because base zones merge first and win on
/// write-class, this can only fill a genuinely-undeclared zone — it can never downgrade a protected
/// base zone.
///
/// Idempotent: an identical grant already present (same component + capability) is a no-op returning
/// `Ok(false)`; a real change returns `Ok(true)`. Writes atomically (temp file + rename) so a crash
/// mid-write can't leave a half-written, unparseable overlay.
pub fn append_grant_to_overlay(component: &str, capability: &Capability) -> std::io::Result<bool> {
    append_grant_to_overlay_at(&grants_overlay_path(), component, capability)
}

/// [`append_grant_to_overlay`] against an explicit path — the testable core.
fn append_grant_to_overlay_at(
    path: &Path,
    component: &str,
    capability: &Capability,
) -> std::io::Result<bool> {
    let mut overlay = load_grants_overlay_at(path);

    // Idempotent: identical grant already present → no change.
    let already = overlay
        .grants
        .iter()
        .any(|g| g.component == component && g.capabilities.iter().any(|c| c == capability));
    if already {
        return Ok(false);
    }

    // Ensure the granted zone is declared, or the merged config would fail validation. Base zones
    // of the same name always win on write-class (merged first), so AgentWritable here is only ever
    // consulted for a zone the base policy never declared.
    if let Some(zone) = capability_zone_name(capability)
        && !overlay.zones.iter().any(|z| z.zone == zone)
    {
        overlay.zones.push(ZonePolicy {
            zone: zone.to_string(),
            write_class: WriteClass::AgentWritable,
        });
    }

    match overlay.grants.iter_mut().find(|g| g.component == component) {
        Some(g) => g.capabilities.push(capability.clone()),
        None => overlay.grants.push(Grant {
            component: component.to_string(),
            capabilities: vec![capability.clone()],
        }),
    }

    // Serialize only zones+grants (both arrays-of-tables). A trailing scalar field like
    // `secret_refs = []` would serialize *after* the tables and trip toml's "values must precede
    // tables" rule — so we use a dedicated write shape rather than serializing `Policy` whole.
    #[derive(serde::Serialize)]
    struct OverlayFile<'a> {
        zones: &'a [ZonePolicy],
        grants: &'a [Grant],
    }
    let toml_body = toml::to_string_pretty(&OverlayFile {
        zones: &overlay.zones,
        grants: &overlay.grants,
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let contents = format!("# {GRANTS_OVERLAY_HEADER}\n\n{toml_body}");

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// The zone a capability names, if any — for [`append_grant_to_overlay_at`]'s zone-declaration
/// guard. `ExecuteMcp`/`AskHuman` name no zone.
fn capability_zone_name(cap: &Capability) -> Option<&str> {
    match cap {
        Capability::Read(z) | Capability::Write(z) | Capability::ReadSummary(z) => match z {
            Zone::Vault(name) | Zone::Named(name) => Some(name),
        },
        Capability::ExecuteMcp(_) | Capability::AskHuman => None,
    }
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
            default_zone: m.default_zone.clone(),
            tool_zones: m
                .tools
                .iter()
                .map(|t| (t.name.clone(), t.zone.clone()))
                .collect(),
            zone_from_arg: m.zone_from_arg.clone(),
            write_tools: m.write_tools.clone(),
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

/// The **converged session store** (S5′/D7): every session — chat, goal session, and background run
/// alike — is one append-only JSONL log in here.
///
/// A function rather than a `.join("sessions")` at each call site, because it had been exactly that,
/// and the two call sites drifted: when chat moved off the old `conversations/` directory, the
/// server's search endpoint followed it and the standalone `chat-search` MCP binary did not — so the
/// agent's own history-search tool went on quietly searching a directory nothing had written to
/// since, finding a frozen archive and none of what the human had actually said. One name, one
/// place, and the next store move can't half-happen.
///
/// The pre-convergence `conversations/` and `goal-sessions/` directories are deliberately left on
/// disk (nothing is destroyed) but are no longer read by anything.
pub fn sessions_dir() -> PathBuf {
    data_dir().join("sessions")
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
    /// MCP descriptors (zone declarations) for the zone-write-class guard (§6 #2) — the same
    /// `catalog.descriptors()` `consequences` above is derived from, passed through directly
    /// rather than reduced to a tuple list, since the zone-resolution helpers already operate on
    /// `McpDescriptor`.
    pub zone_catalog: Vec<liberado_common::McpDescriptor>,
    /// `(zone, write_class)` pairs from `Policy.zones` — what a resolved zone is checked against.
    pub zone_write_classes: Vec<(String, liberado_common::WriteClass)>,
    pub proposals_dir: PathBuf,
    pub signer: liberado_common::ProposalSigner,
}

/// Build a [`GuardContext`] from the live capability catalog, the policy (for zone write-classes),
/// and the vault path a runtime-level proposal downgrade should be written under.
pub fn guard_context(
    catalog: &CapabilityCatalog,
    policy: &liberado_config_loader::Policy,
    vault_path: &Path,
) -> GuardContext {
    GuardContext {
        consequences: catalog.consequence_catalog(),
        zone_catalog: catalog.descriptors(),
        zone_write_classes: policy
            .zones
            .iter()
            .map(|z| (z.zone.clone(), z.write_class))
            .collect(),
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
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() == PROPOSAL_KEY_LEN
    {
        return bytes;
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
writes_vault = false   # F1: a writing MCP must say what it touches, even in a fixture
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
                default_zone: None,
                tools: Vec::new(),
                zone_from_arg: None,
                write_tools: Vec::new(),
                writes_vault: Some(false),
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
                default_zone: None,
                tools: Vec::new(),
                zone_from_arg: None,
                write_tools: Vec::new(),
                writes_vault: Some(false),
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
writes_vault = false   # sends email; writes no vault zone
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

    // ── config_dir()'s 4-tier resolution order (resolve_config_dir, the pure helper) ──

    fn dir_with_topology(dir: &Path) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        write_file(dir, TOPOLOGY_FILE, TOPOLOGY_TOML);
        dir.to_path_buf()
    }

    #[test]
    fn env_dir_wins_over_everything_else() {
        let platform = TempDir::new().unwrap();
        let env = PathBuf::from("/explicit/env/dir");
        let resolved = resolve_config_dir(
            Some(env.clone()),
            Some(dir_with_topology(platform.path())),
            None,
        );
        assert_eq!(resolved, Some(env));
    }

    #[test]
    fn empty_env_dir_is_treated_as_absent() {
        let platform = TempDir::new().unwrap();
        let populated = dir_with_topology(platform.path());
        let resolved = resolve_config_dir(Some(PathBuf::new()), Some(populated.clone()), None);
        assert_eq!(resolved, Some(populated));
    }

    #[test]
    fn platform_dir_wins_when_populated() {
        let platform = TempDir::new().unwrap();
        let populated = dir_with_topology(platform.path());
        let resolved = resolve_config_dir(None, Some(populated.clone()), None);
        assert_eq!(resolved, Some(populated));
    }

    #[test]
    fn empty_platform_dir_falls_through_to_exe_walk_up() {
        let platform = TempDir::new().unwrap(); // no config file written — empty
        let exe_root = TempDir::new().unwrap();
        let exe_dir = exe_root.path().join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&exe_dir).unwrap();
        let config_dir = dir_with_topology(&exe_root.path().join("config"));

        let resolved = resolve_config_dir(None, Some(platform.path().to_path_buf()), Some(exe_dir));
        assert_eq!(resolved, Some(config_dir));
    }

    #[test]
    fn exe_walk_up_stops_after_five_levels() {
        let platform = TempDir::new().unwrap(); // no config file — the eventual fallback
        let exe_root = TempDir::new().unwrap();
        // One level deeper than `empty_platform_dir_falls_through_to_exe_walk_up`'s passing case —
        // puts the config dir just past the 5-ancestor walk-up limit.
        let exe_dir = exe_root
            .path()
            .join("a")
            .join("b")
            .join("c")
            .join("d")
            .join("e");
        std::fs::create_dir_all(&exe_dir).unwrap();
        dir_with_topology(&exe_root.path().join("config"));

        let resolved = resolve_config_dir(None, Some(platform.path().to_path_buf()), Some(exe_dir));
        // Never found within 5 levels — falls back to tier 4 (platform dir, even though empty).
        assert_eq!(resolved, Some(platform.path().to_path_buf()));
    }

    #[test]
    fn everything_absent_falls_back_to_empty_platform_dir() {
        let platform = TempDir::new().unwrap(); // no config file
        let resolved = resolve_config_dir(None, Some(platform.path().to_path_buf()), None);
        assert_eq!(resolved, Some(platform.path().to_path_buf()));
    }

    #[test]
    fn everything_none_resolves_to_none() {
        assert_eq!(resolve_config_dir(None, None, None), None);
    }

    #[test]
    fn the_session_store_directory_has_exactly_one_definition() {
        // Regression: this was a `.join("sessions")` at each call site, and the sites drifted. When
        // chat moved onto the converged store, `liberado-server`'s search endpoint followed and the
        // standalone `chat-search` MCP binary did not — it went on searching `conversations/`, a
        // directory nothing had written to since, so the agent's own history search quietly returned
        // a frozen archive and nothing the human had said since. Every reader of the session logs
        // must come through this one function.
        assert_eq!(sessions_dir(), data_dir().join("sessions"));
    }

    // --- grants overlay (machine-owned "Approve everywhere" persistence) ---------------------

    #[test]
    fn append_grant_to_overlay_persists_and_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("grants.overlay.toml");

        let changed = append_grant_to_overlay_at(
            &path,
            "dispatcher",
            &Capability::Write(Zone::vault("sandbox")),
        )
        .unwrap();
        assert!(changed, "first append must report a change");
        assert!(path.exists(), "the overlay file must be written");

        let overlay = load_grants_overlay_at(&path);
        let grant = overlay
            .grants
            .iter()
            .find(|g| g.component == "dispatcher")
            .expect("dispatcher grant persisted");
        assert!(
            grant
                .capabilities
                .contains(&Capability::Write(Zone::vault("sandbox"))),
            "the granted capability round-trips"
        );
        // The undeclared zone was auto-declared so the merged config stays valid.
        assert!(
            overlay
                .zones
                .iter()
                .any(|z| z.zone == "sandbox" && z.write_class == WriteClass::AgentWritable),
            "an undeclared granted zone is declared AgentWritable in the overlay"
        );
    }

    #[test]
    fn append_grant_to_overlay_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("grants.overlay.toml");
        let cap = Capability::Write(Zone::vault("sandbox"));

        assert!(append_grant_to_overlay_at(&path, "dispatcher", &cap).unwrap());
        // Second identical append is a no-op.
        assert!(!append_grant_to_overlay_at(&path, "dispatcher", &cap).unwrap());

        let overlay = load_grants_overlay_at(&path);
        let grant = overlay
            .grants
            .iter()
            .find(|g| g.component == "dispatcher")
            .unwrap();
        assert_eq!(
            grant.capabilities.iter().filter(|c| **c == cap).count(),
            1,
            "the capability is not duplicated on a repeat approval"
        );
        assert_eq!(
            overlay.zones.iter().filter(|z| z.zone == "sandbox").count(),
            1,
            "the zone declaration is not duplicated either"
        );
    }

    #[test]
    fn append_grant_to_overlay_accumulates_distinct_grants() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("grants.overlay.toml");

        append_grant_to_overlay_at(
            &path,
            "dispatcher",
            &Capability::Write(Zone::vault("sandbox")),
        )
        .unwrap();
        append_grant_to_overlay_at(
            &path,
            "dispatcher",
            &Capability::Write(Zone::vault("scratch")),
        )
        .unwrap();
        append_grant_to_overlay_at(&path, "life", &Capability::Write(Zone::vault("sandbox")))
            .unwrap();

        let overlay = load_grants_overlay_at(&path);
        let dispatcher = overlay
            .grants
            .iter()
            .find(|g| g.component == "dispatcher")
            .unwrap();
        assert_eq!(dispatcher.capabilities.len(), 2, "two zones for dispatcher");
        assert!(
            overlay.grants.iter().any(|g| g.component == "life"),
            "a second component gets its own grant entry"
        );
    }

    #[test]
    fn a_missing_overlay_is_an_empty_policy_not_an_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let overlay = load_grants_overlay_at(&path);
        assert!(overlay.grants.is_empty() && overlay.zones.is_empty());
    }

    #[test]
    fn a_corrupt_overlay_is_ignored_not_fatal() {
        // Fail-safe: a machine overlay that somehow got mangled must never brick boot — it degrades
        // to "grant nothing extra", not an error.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("grants.overlay.toml");
        std::fs::write(&path, b"this is not valid toml {{{").unwrap();
        let overlay = load_grants_overlay_at(&path);
        assert!(overlay.grants.is_empty() && overlay.zones.is_empty());
    }

    #[test]
    fn merged_overlay_grant_validates_against_the_config_checks() {
        // The whole point of auto-declaring the zone: an "everywhere" grant for a zone the base
        // policy never declared must produce a config that still passes validate_merged_config
        // (which requires every granted zone be declared). Simulate the load_config merge.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("grants.overlay.toml");
        append_grant_to_overlay_at(
            &path,
            "dispatcher",
            &Capability::Write(Zone::vault("sandbox")),
        )
        .unwrap();
        let overlay = load_grants_overlay_at(&path);

        // A minimal base config with a dispatcher grant but NO sandbox zone.
        let mut config = Config::default();
        config.topology.vault_path = "/tmp/vault".into();
        let mut policy = Policy::default();
        merge_overlay_into(&mut policy, overlay);
        config.policy = policy;

        liberado_config_loader::validate_merged_config(&config)
            .expect("the auto-declared zone keeps the merged config valid");
    }

    #[test]
    fn base_zone_write_class_wins_over_the_overlay() {
        // Safety: an overlay declaring `sandbox = AgentWritable` must never override a base policy
        // that (hypothetically) declared the same zone `human_only`. Base merges first; write_class
        // scans in order and takes the first match.
        let mut policy = Policy {
            zones: vec![ZonePolicy {
                zone: "sandbox".into(),
                write_class: WriteClass::HumanOnly,
            }],
            ..Policy::default()
        };
        let overlay = Policy {
            zones: vec![ZonePolicy {
                zone: "sandbox".into(),
                write_class: WriteClass::AgentWritable,
            }],
            ..Policy::default()
        };
        merge_overlay_into(&mut policy, overlay);
        assert_eq!(
            policy.write_class("sandbox"),
            WriteClass::HumanOnly,
            "the base zone's protection must not be downgraded by the overlay"
        );
    }
}
