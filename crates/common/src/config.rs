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
//! ACPs) is the daemon's job; [`Config::validate`] here covers the model-level invariants that
//! can be checked from these types alone.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, WriteClass};
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
    /// Enabled MCP servers.
    pub mcps: Vec<ComponentConfig>,
    /// Enabled ACP webhook receivers.
    pub acps: Vec<ComponentConfig>,
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
            acps: Vec::new(),
        }
    }
}

/// A wired component (MCP or ACP): how it's reached and whether it's on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Transport endpoint: a socket path, `http://…`, or webhook URL depending on component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonePolicy {
    /// Zone name (a vault folder, or a named external zone).
    pub zone: String,
    pub write_class: WriteClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    /// Component the grant applies to (MCP/ACP/subagent role name).
    pub component: String,
    pub capabilities: Vec<Capability>,
}

// ---------------------------------------------------------------------------
// Tuning — benign behavior knobs. Every field defaults to its specced value.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tuning {
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

impl Config {
    /// Validate invariants checkable from the resolved model alone. The daemon's loader layers
    /// additional cross-cutting checks on top (port/socket collisions, dangling zone/secret
    /// refs, triggerless ACPs). Returns the first violation found.
    pub fn validate(&self) -> Result<()> {
        if self.topology.vault_path.as_os_str().is_empty() {
            return Err(Error::Config("topology.vault_path is required".into()));
        }
        if self.tuning.dispatch.max_concurrent_subagents == 0 {
            return Err(Error::Config(
                "tuning.dispatch.max_concurrent_subagents must be >= 1".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelTier;

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
}
