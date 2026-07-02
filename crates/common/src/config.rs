//! The typed config model (Decision 14, `liberado-config-spec.md`).
//!
//! Single source of truth = one resolved, validated *model* — not one file. The daemon merges
//! many small files (defaults → files → env → CLI) into this. Two principles are baked into the
//! type design:
//!
//! 1. **Defaults live in code; config holds only deltas.** Every tunable's [`Default`] is the
//!    value specced in its home document, so an empty/absent config still boots. Each spec's
//!    "Tunables (single source of truth)" table is mirrored here as typed fields.
//! 2. **Each setting is owned by exactly one place.** Where a tunable is shared across specs
//!    (e.g. `MAX_REACTION_DEPTH`), it lives in *one* sub-struct and others reference it; this
//!    module never declares the same knob twice.
//!
//! Durations are stored as plain seconds (`*_secs`) for unambiguous TOML/serde. The actual
//! file loader + cross-cutting validation (port collisions, dangling zone refs, triggerless
//! hooks) is the daemon's job; [`Config::validate`] here covers the model-level invariants that
//! can be checked from these types alone.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, CapabilitySet, Consequence, WriteClass};
use crate::error::{Error, Result};
use crate::model::{ModelProfile, ModelRole};

/// The fully-resolved configuration the daemon runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub topology: Topology,
    pub policy: Policy,
    pub tuning: Tuning,
}

// ---------------------------------------------------------------------------
// Topology — wiring (homelab-local). No universal Default for deployment-specific
// fields like the vault path; `validate` enforces their presence.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Topology {
    /// Path to the Obsidian vault (the source of truth). Required.
    pub vault_path: PathBuf,
    /// Unix domain socket the daemon listens on for TUI/client attach (Decision 2).
    pub daemon_socket: PathBuf,
    /// Inference provider name (provider-agnostic scaffolding, Decision 9/13).
    pub provider: String,
    /// Declared model profiles available to the system.
    pub models: Vec<ModelProfile>,
    /// Which model (by name) fills each role. Validated against the capability floors.
    pub model_roles: HashMap<ModelRole, String>,
    /// Enabled MCP servers (each carries the routing + risk metadata the dispatcher needs).
    pub mcps: Vec<McpConfig>,
    /// Enabled hook (webhook) receivers.
    pub hooks: Vec<ComponentConfig>,
}

impl Default for Topology {
    fn default() -> Self {
        Self {
            vault_path: PathBuf::new(),
            daemon_socket: PathBuf::from("/run/liberado/daemon.sock"),
            provider: "deepseek".to_string(),
            models: Vec::new(),
            model_roles: HashMap::new(),
            mcps: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

/// A wired component (hook): how it's reached and whether it's on. MCPs use [`McpConfig`], which
/// additionally carries the routing description + risk rating the dispatcher needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Transport endpoint: a socket path, `http://…`, or webhook URL depending on component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// A wired MCP server: how it's reached, plus the routing description and risk classification the
/// dispatcher needs. Description and consequence are REQUIRED — declaring an MCP means rating it.
/// (Liberado owns risk classification; MCPs don't declare their own risk, and `Consequence::default()`
/// is the *unsafe* `ReadOnly`, so we never let it default silently.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Short description the dispatcher routes over.
    pub description: String,
    /// Our reversibility/externality rating; the consequence guard gates on it.
    pub consequence: Consequence,
    /// How the runtime actually reaches this server. Same source (`topology.mcps`) drives both the
    /// dispatcher catalog and the connection, so a name routed to is a name we can connect to.
    pub transport: McpTransport,
}

/// How to reach an MCP server. Stdio spawns a child process; Http connects to a URL (Decision 3).
/// Adjacently tagged so the variant key is a plain `kind` field — that round-trips cleanly through
/// TOML inline tables (`transport = { kind = "stdio", command = "npx", args = [...] }`), which an
/// internally-tagged enum does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpTransport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        url: String,
    },
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Policy — the central, auditable security surface (Decision 4 / 5).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Per-zone write classes. An *unlisted* zone is treated as `proposal_only` (fail safe).
    pub zones: Vec<ZonePolicy>,
    /// Base capability grants per component (narrowed, never widened, at dispatch).
    pub grants: Vec<Grant>,
    /// Names of secrets referenced by components (resolved from env/systemd, never inlined).
    pub secret_refs: Vec<String>,
}

impl Policy {
    /// The write class declared for `zone`, or the fail-safe default if unlisted.
    pub fn write_class(&self, zone: &str) -> WriteClass {
        self.zones
            .iter()
            .find(|z| z.zone == zone)
            .map(|z| z.write_class)
            .unwrap_or_default()
    }

    /// The daemon's base capability set: the union of all granted capabilities. The dispatcher
    /// holds this maximal authority and only ever NARROWS it per dispatch (Decision 4 invariant).
    /// v1 unions across components; per-component narrowing for subagents is a later refinement.
    pub fn base_capabilities(&self) -> CapabilitySet {
        self.grants
            .iter()
            .flat_map(|g| g.capabilities.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonePolicy {
    /// Zone name (a vault folder, or a named external zone).
    pub zone: String,
    pub write_class: WriteClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    /// Component the grant applies to (MCP/hook/subagent role name).
    pub component: String,
    pub capabilities: Vec<Capability>,
}

// ---------------------------------------------------------------------------
// Tuning — benign behavior knobs. Every field defaults to its specced value.
// ---------------------------------------------------------------------------

/// The current schema version for `tuning.toml`. Used by the loader to warn when a user's
/// tuning file carries a different version (e.g. after an upgrade). Bump this when a
/// backward-incompatible change is made to the tuning schema.
pub const CURRENT_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
    /// Optional schema version marker for deprecation detection. When set, the loader compares
    /// this against [`CURRENT_SCHEMA_VERSION`] and warns if they differ. Absent => no check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    pub dispatch: DispatchTuning,
    pub context: ContextTuning,
    pub concurrency: ConcurrencyTuning,
    pub capture: CaptureTuning,
    pub maintenance: MaintenanceTuning,
}

/// Subagent isolation level (Decision 8). Configurable so scaling to process isolation is a
/// config change, not a source edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentIsolation {
    #[default]
    InProcess,
    OutOfProcess,
}

/// Dispatch tunables (`liberado-dispatch-logic-spec.md` §11). Note: `MAX_REACTION_DEPTH` is
/// *not* here — it is owned by [`ConcurrencyTuning::max_reaction_depth`] and shared.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchTuning {
    /// Max tool calls allowed in `ExecuteDirect`; more ⇒ multi-step ⇒ subagent.
    pub small_fanout: u32,
    /// Below this confidence on a read-only action, downgrade to `Clarify`. Reserved: the guard
    /// currently applies `clarify_threshold_write` to every action-taking decision (conservative);
    /// read/write tiering is wired in once per-tool read/write metadata lands.
    pub clarify_threshold_read: f32,
    /// Higher bar before an `agent_writable` write.
    pub clarify_threshold_write: f32,
    /// In-flight subagent cap (KV-cache / homelab bound); excess queues.
    pub max_concurrent_subagents: u32,
    /// Await → Detach promotion point for foreground dispatches (seconds).
    pub detach_soft_timeout_secs: u64,
    /// Procedural-memory score above which the classification short-circuit fires.
    pub guidance_match_floor: f32,
    pub subagent_isolation: SubagentIsolation,
    /// Resource cap for in-flight code-dispatch (riggers) jobs. Not a safety gate — the capability
    /// grant is the authority gate; this limits build-job churn on homelab hardware.
    pub max_concurrent_coding_subagents: u32,
}

impl Default for DispatchTuning {
    fn default() -> Self {
        Self {
            small_fanout: 3,
            clarify_threshold_read: 0.5,
            clarify_threshold_write: 0.7,
            max_concurrent_subagents: 2,
            detach_soft_timeout_secs: 20,
            guidance_match_floor: 0.8,
            subagent_isolation: SubagentIsolation::InProcess,
            max_concurrent_coding_subagents: 2,
        }
    }
}

/// ContextPolicy tunables (`liberado-context-policy-spec.md` §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextTuning {
    /// Active goals shown in the header.
    pub max_goals: u32,
    /// Recent high-signal decisions shown.
    pub max_decisions: u32,
    /// Window (days) for "recent" decisions (plus any `important`-tagged).
    pub decision_recency_days: u32,
}

impl Default for ContextTuning {
    fn default() -> Self {
        Self {
            max_goals: 5,
            max_decisions: 5,
            decision_recency_days: 7,
        }
    }
}

/// Concurrency / loop-breaking tunables (`liberado-vault-concurrency-spec.md` §6.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ConcurrencyTuning {
    /// Recency window (seconds) for "this audit entry explains this event".
    pub window_secs: u64,
    /// Max correlation-chain depth before the daemon halts a cascade (shared with dispatch).
    pub max_reaction_depth: u32,
    /// Optimistic-write retries before escalating.
    pub retry_max: u32,
}

impl Default for ConcurrencyTuning {
    fn default() -> Self {
        Self {
            window_secs: 60,
            max_reaction_depth: 4,
            retry_max: 3,
        }
    }
}

/// Capture / inbox tunables (`liberado-inbox-spec.md` §11).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureTuning {
    pub inbox_path: String,
    pub processed_path: String,
    /// Quiescence before an actionable note is processed (seconds).
    pub inbox_settle_window_secs: u64,
    /// Shorter quiescence for `#ready-now` notes (seconds).
    pub ready_now_settle_secs: u64,
    pub ready_flag: String,
    pub hold_flag: String,
    /// Cron-ish schedule for the low-intensity whole-vault ambient sweep.
    pub ambient_sweep_schedule: String,
    /// Never-process patterns (Syncthing/editor artifacts).
    pub inbox_ignore_globs: Vec<String>,
}

impl Default for CaptureTuning {
    fn default() -> Self {
        Self {
            inbox_path: "inbox/".to_string(),
            processed_path: "processed/".to_string(),
            inbox_settle_window_secs: 15 * 60,
            ready_now_settle_secs: 2 * 60,
            ready_flag: "#ready-now".to_string(),
            hold_flag: "#hold-off".to_string(),
            ambient_sweep_schedule: "nightly".to_string(),
            inbox_ignore_globs: vec![
                "*.sync-conflict-*".to_string(),
                ".stversions/".to_string(),
                "*.tmp".to_string(),
                "~*".to_string(),
            ],
        }
    }
}

/// Vault maintenance + git tunables (`liberado-vault-maintenance-and-git-spec.md` §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MaintenanceTuning {
    pub git_commit_schedule: String,
    /// Dirs Syncthing must not replicate (the `.git/` footgun + machine-managed dirs).
    pub stignore_machine_dirs: Vec<String>,
    pub maintenance_schedule: String,
    /// Pruning human-authored content always proposes first.
    pub prune_requires_proposal: bool,
}

impl Default for MaintenanceTuning {
    fn default() -> Self {
        Self {
            git_commit_schedule: "per-batch+hourly".to_string(),
            stignore_machine_dirs: vec![
                ".git/".to_string(),
                ".turbovault/".to_string(),
                ".liberado/".to_string(),
            ],
            maintenance_schedule: "weekly".to_string(),
            prune_requires_proposal: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation — the model-level slice of the Decision 14 fail-fast contract.
// ---------------------------------------------------------------------------

impl std::str::FromStr for Config {
    type Err = Error;

    /// Parse a TOML string and overlay it on [`Config::default()`].
    ///
    /// Any keys present in the TOML override the built-in defaults; absent keys keep
    /// their default values. After deserialization the result is validated via
    /// [`Config::validate`].
    ///
    /// # Errors
    ///
    /// - Returns [`Error::Config`] if the TOML is malformed or fails to deserialize.
    /// - Returns the first validation error from [`Config::validate`].
    fn from_str(toml_str: &str) -> Result<Self> {
        let config: Config =
            toml::from_str(toml_str).map_err(|e| Error::Config(format!("parse error: {e}")))?;
        config.validate()?;
        Ok(config)
    }
}

impl Config {
    /// Return a [`ConfigBuilder`] initialised with [`Config::default()`].
    ///
    /// The builder provides ergonomic, chainable setters for test construction and
    /// programmatic config creation. Call `.build()` to validate and produce the final
    /// [`Config`].
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }

    /// Validate invariants checkable from the resolved model alone. The daemon's loader layers
    /// additional cross-cutting checks on top (port/socket collisions, dangling zone/secret
    /// refs, triggerless hooks). Returns the first violation found.
    pub fn validate(&self) -> Result<()> {
        if self.topology.vault_path.as_os_str().is_empty() {
            return Err(Error::Config("topology.vault_path is required".into()));
        }
        if self.tuning.dispatch.max_concurrent_subagents == 0 {
            return Err(Error::Config(
                "tuning.dispatch.max_concurrent_subagents must be >= 1".into(),
            ));
        }
        if self.tuning.dispatch.max_concurrent_coding_subagents == 0 {
            return Err(Error::Config(
                "tuning.dispatch.max_concurrent_coding_subagents must be >= 1".into(),
            ));
        }
        if self.tuning.concurrency.max_reaction_depth == 0 {
            return Err(Error::Config(
                "tuning.concurrency.max_reaction_depth must be >= 1".into(),
            ));
        }

        // Every role assignment must name a declared model that meets the role's floor (D13).
        for (role, model_name) in &self.topology.model_roles {
            let profile = self
                .topology
                .models
                .iter()
                .find(|m| &m.name == model_name)
                .ok_or_else(|| {
                    Error::Config(format!(
                        "model_roles[{}] references undeclared model '{}'",
                        role.as_str(),
                        model_name
                    ))
                })?;
            if !profile.meets(*role) {
                return Err(Error::ModelCapabilityFloor {
                    model: model_name.clone(),
                    role: role.as_str().to_string(),
                });
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ConfigBuilder — ergonomic programmatic construction for tests and wiring.
// ---------------------------------------------------------------------------

/// A builder for constructing [`Config`] values programmatically.
///
/// Start with [`Config::builder()`], chain setters, and finish with
/// [`build`](ConfigBuilder::build) which validates the assembled config.
///
/// # Example
///
/// ```rust
/// use liberado_common::config::Config;
///
/// let cfg = Config::builder()
///     .vault_path("/home/test/vault")
///     .provider("deepseek")
///     .build()
///     .expect("valid config");
/// ```
#[derive(Debug, Clone, Default)]
pub struct ConfigBuilder {
    config: Config,
}

impl ConfigBuilder {
    // ── topology setters ────────────────────────────────────────────────────

    /// Set the vault path (required; validation will fail if empty).
    pub fn vault_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.topology.vault_path = path.into();
        self
    }

    /// Set the daemon socket path.
    pub fn daemon_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.topology.daemon_socket = path.into();
        self
    }

    /// Set the inference provider name.
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.config.topology.provider = provider.into();
        self
    }

    /// Add a model profile.
    pub fn model(mut self, model: crate::model::ModelProfile) -> Self {
        self.config.topology.models.push(model);
        self
    }

    /// Assign a model to a role (replaces any existing assignment for that role).
    pub fn model_role(mut self, role: crate::model::ModelRole, name: impl Into<String>) -> Self {
        self.config.topology.model_roles.insert(role, name.into());
        self
    }

    /// Add an MCP server config.
    pub fn mcp(mut self, mcp: McpConfig) -> Self {
        self.config.topology.mcps.push(mcp);
        self
    }

    /// Add a hook component config.
    pub fn hook(mut self, hook: ComponentConfig) -> Self {
        self.config.topology.hooks.push(hook);
        self
    }

    // ── policy setters ──────────────────────────────────────────────────────

    /// Add a zone policy entry.
    pub fn zone(mut self, zone: ZonePolicy) -> Self {
        self.config.policy.zones.push(zone);
        self
    }

    /// Add a capability grant.
    pub fn grant(mut self, grant: Grant) -> Self {
        self.config.policy.grants.push(grant);
        self
    }

    /// Add a secret reference.
    pub fn secret_ref(mut self, secret: impl Into<String>) -> Self {
        self.config.policy.secret_refs.push(secret.into());
        self
    }

    // ── tuning setters (convenience for the most commonly-overridden fields) ─

    /// Override the tuning section wholesale.
    pub fn tuning(mut self, tuning: Tuning) -> Self {
        self.config.tuning = tuning;
        self
    }

    /// Set the schema version marker.
    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.config.tuning.schema_version = Some(version.into());
        self
    }

    // ── finish ──────────────────────────────────────────────────────────────

    /// Validate and return the constructed [`Config`].
    ///
    /// # Errors
    ///
    /// Delegates to [`Config::validate`]; returns the first validation error.
    pub fn build(self) -> Result<Config> {
        self.config.validate()?;
        Ok(self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Zone;
    use crate::model::{ModelProfile, ModelRole, ModelTier};
    use std::str::FromStr;

    #[test]
    fn base_capabilities_unions_grants_and_dedups() {
        let policy = Policy {
            zones: Vec::new(),
            grants: vec![
                Grant {
                    component: "tasks-mcp".into(),
                    capabilities: vec![
                        Capability::Read(Zone::vault("tasks")),
                        Capability::Write(Zone::vault("tasks")),
                    ],
                },
                Grant {
                    component: "memory-mcp".into(),
                    capabilities: vec![
                        // Overlaps with the first grant — the union must de-duplicate.
                        Capability::Read(Zone::vault("tasks")),
                        Capability::ExecuteMcp("memory-mcp".into()),
                    ],
                },
            ],
            secret_refs: Vec::new(),
        };

        let caps = policy.base_capabilities();
        assert!(caps.contains(&Capability::Read(Zone::vault("tasks"))));
        assert!(caps.contains(&Capability::Write(Zone::vault("tasks"))));
        assert!(caps.contains(&Capability::ExecuteMcp("memory-mcp".into())));
        // Read(tasks) appeared twice across grants but is held once.
        assert_eq!(caps.capabilities.len(), 3);
    }

    #[test]
    fn defaults_match_specced_values() {
        let t = Tuning::default();
        assert_eq!(t.dispatch.small_fanout, 3);
        assert_eq!(t.dispatch.clarify_threshold_read, 0.5);
        assert_eq!(t.dispatch.clarify_threshold_write, 0.7);
        assert_eq!(t.dispatch.max_concurrent_subagents, 2);
        assert_eq!(t.dispatch.detach_soft_timeout_secs, 20);
        assert_eq!(t.context.max_goals, 5);
        assert_eq!(t.context.decision_recency_days, 7);
        assert_eq!(t.concurrency.window_secs, 60);
        assert_eq!(t.concurrency.max_reaction_depth, 4);
        assert_eq!(t.concurrency.retry_max, 3);
        assert_eq!(t.capture.inbox_settle_window_secs, 900);
        assert_eq!(t.capture.ready_now_settle_secs, 120);
        assert!(t.maintenance.prune_requires_proposal);
    }

    #[test]
    fn empty_config_needs_a_vault_path() {
        let cfg = Config::default();
        assert!(cfg.validate().is_err(), "empty config must fail validation");
    }

    #[test]
    fn minimal_valid_config_passes() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn mcp_transport_both_variants_deserialize_from_toml() {
        // Stdio + Http must each round-trip through a TOML `[[mcps]]` inline table — the runtime
        // builds connectors directly off this, so the representation has to load from real config.
        let toml = r#"
[[mcps]]
name = "tasks-mcp"
description = "create and complete tasks"
consequence = "reversible"
transport = { kind = "stdio", command = "npx", args = ["-y", "@scope/tasks"] }

[[mcps]]
name = "wiki-mcp"
description = "query external docs"
consequence = "read_only"
transport = { kind = "http", url = "https://mcp.deepwiki.com/mcp" }
"#;
        let topology: Topology = toml::from_str(toml).expect("transport variants must deserialize");
        assert_eq!(topology.mcps.len(), 2);
        match &topology.mcps[0].transport {
            McpTransport::Stdio { command, args } => {
                assert_eq!(command, "npx");
                assert_eq!(args, &["-y", "@scope/tasks"]);
            }
            other => panic!("expected stdio, got {other:?}"),
        }
        match &topology.mcps[1].transport {
            McpTransport::Http { url } => assert_eq!(url, "https://mcp.deepwiki.com/mcp"),
            other => panic!("expected http, got {other:?}"),
        }
    }

    #[test]
    fn rejects_model_that_misses_role_floor() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/vault");
        cfg.topology.models.push(ModelProfile {
            name: "text-only".into(),
            tool_calling: false,
            structured_output: false,
            context_window: 8000,
            tier: ModelTier::ControlPlane,
            cost: None,
        });
        cfg.topology
            .model_roles
            .insert(ModelRole::Dispatcher, "text-only".into());

        assert!(matches!(
            cfg.validate(),
            Err(Error::ModelCapabilityFloor { .. })
        ));
    }

    // ── Config::from_str tests ──────────────────────────────────────────────

    #[test]
    fn from_str_parses_valid_toml() {
        let toml = r#"
[topology]
vault_path = "/home/test/vault"
provider = "test-provider"

[[topology.mcps]]
name = "my-mcp"
description = "a test MCP"
consequence = "read_only"
transport = { kind = "stdio", command = "echo", args = ["hello"] }
"#;
        let cfg = Config::from_str(toml).expect("valid TOML should parse");
        assert_eq!(cfg.topology.vault_path, PathBuf::from("/home/test/vault"));
        assert_eq!(cfg.topology.provider, "test-provider");
        assert_eq!(cfg.topology.mcps.len(), 1);
        assert_eq!(cfg.topology.mcps[0].name, "my-mcp");

        // Fields not in the TOML keep their defaults
        assert_eq!(cfg.tuning.dispatch.small_fanout, 3);
        assert!(cfg.policy.zones.is_empty());
    }

    #[test]
    fn from_str_accepts_empty_toml_as_defaults() {
        let cfg = Config::from_str("");
        // All defaults → validation fails because vault_path is empty
        assert!(cfg.is_err(), "empty TOML should parse but fail validation");
        let msg = cfg.unwrap_err().to_string();
        assert!(msg.contains("vault_path"), "got: {msg}");
    }

    #[test]
    fn from_str_rejects_malformed_toml() {
        let err = Config::from_str("not valid toml {{{").unwrap_err();
        assert!(
            matches!(err, Error::Config(_)),
            "expected Config error, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("parse error"), "got: {msg}");
    }

    #[test]
    fn from_str_rejects_config_with_missing_vault_path() {
        let toml = r#"
[topology]
provider = "deepseek"
"#;
        let err = Config::from_str(toml).unwrap_err();
        assert!(
            matches!(err, Error::Config(_)),
            "expected Config error, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("vault_path"), "got: {msg}");
    }

    #[test]
    fn from_str_overrides_tuning_defaults() {
        let toml = r#"
[topology]
vault_path = "/vault"

[tuning.dispatch]
small_fanout = 10
clarify_threshold_read = 0.8
"#;
        let cfg = Config::from_str(toml).expect("valid TOML");
        assert_eq!(cfg.tuning.dispatch.small_fanout, 10);
        assert_eq!(cfg.tuning.dispatch.clarify_threshold_read, 0.8);
        // Unset tuning fields keep defaults
        assert_eq!(cfg.tuning.dispatch.clarify_threshold_write, 0.7);
        assert_eq!(cfg.tuning.context.max_goals, 5);
    }

    // ── ConfigBuilder tests ─────────────────────────────────────────────────

    #[test]
    fn builder_minimal_valid_config() {
        let cfg = Config::builder()
            .vault_path("/home/test/vault")
            .build()
            .expect("minimal config should validate");
        assert_eq!(cfg.topology.vault_path, PathBuf::from("/home/test/vault"));
        assert_eq!(cfg.topology.provider, "deepseek"); // default
    }

    #[test]
    fn builder_sets_topology_fields() {
        let cfg = Config::builder()
            .vault_path("/vault")
            .daemon_socket("/tmp/test.sock")
            .provider("custom")
            .build()
            .expect("valid config");
        assert_eq!(cfg.topology.daemon_socket, PathBuf::from("/tmp/test.sock"));
        assert_eq!(cfg.topology.provider, "custom");
    }

    #[test]
    fn builder_adds_models_and_roles() {
        let cfg = Config::builder()
            .vault_path("/vault")
            .model(ModelProfile {
                name: "my-model".into(),
                tool_calling: true,
                structured_output: true,
                context_window: 16000,
                tier: ModelTier::ControlPlane,
                cost: None,
            })
            .model_role(ModelRole::Dispatcher, "my-model")
            .build()
            .expect("model profile and role should validate");
        assert_eq!(cfg.topology.models.len(), 1);
        assert_eq!(cfg.topology.models[0].name, "my-model");
        assert_eq!(
            cfg.topology.model_roles.get(&ModelRole::Dispatcher),
            Some(&"my-model".to_string())
        );
    }

    #[test]
    fn builder_adds_mcp_and_hook() {
        let cfg = Config::builder()
            .vault_path("/vault")
            .mcp(McpConfig {
                name: "mcp1".into(),
                enabled: true,
                description: "test MCP".into(),
                consequence: Consequence::Reversible,
                transport: McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@scope/mcp".into()],
                },
            })
            .hook(ComponentConfig {
                name: "hook1".into(),
                enabled: true,
                endpoint: Some("http://localhost:9000".into()),
            })
            .build()
            .expect("valid config");
        assert_eq!(cfg.topology.mcps.len(), 1);
        assert_eq!(cfg.topology.hooks.len(), 1);
        assert_eq!(cfg.topology.mcps[0].name, "mcp1");
        assert_eq!(cfg.topology.hooks[0].name, "hook1");
    }

    #[test]
    fn builder_adds_policy_items() {
        let cfg = Config::builder()
            .vault_path("/vault")
            .zone(ZonePolicy {
                zone: "tasks".into(),
                write_class: WriteClass::AgentWritable,
            })
            .grant(Grant {
                component: "agent".into(),
                capabilities: vec![
                    Capability::Read(Zone::vault("tasks")),
                    Capability::Write(Zone::vault("tasks")),
                ],
            })
            .secret_ref("MY_SECRET")
            .build()
            .expect("valid config");
        assert_eq!(cfg.policy.zones.len(), 1);
        assert_eq!(cfg.policy.grants.len(), 1);
        assert_eq!(cfg.policy.secret_refs, vec!["MY_SECRET"]);
    }

    #[test]
    fn builder_rejects_missing_vault_path() {
        let err = Config::builder().build().unwrap_err();
        assert!(
            matches!(err, Error::Config(_)),
            "expected Config error, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("vault_path"), "got: {msg}");
    }

    #[test]
    fn builder_tuning_override() {
        let cfg = Config::builder()
            .vault_path("/vault")
            .schema_version("2.0")
            .build()
            .expect("valid config");
        assert_eq!(cfg.tuning.schema_version, Some("2.0".to_string()));
    }

    #[test]
    fn builder_tuning_wholesale() {
        let mut tuning = Tuning::default();
        tuning.dispatch.small_fanout = 99;

        let cfg = Config::builder()
            .vault_path("/vault")
            .tuning(tuning)
            .build()
            .expect("valid config");
        assert_eq!(cfg.tuning.dispatch.small_fanout, 99);
    }
}
