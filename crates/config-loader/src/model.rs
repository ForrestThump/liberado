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
//! hooks) is `liberado-config`'s job (file loading) and this crate's [`crate::validate_merged_config`]
//! (cross-cutting checks); [`Config::validate`] here covers the model-level invariants that
//! can be checked from these types alone.
//!
//! Lives in `liberado-config-loader`, not `liberado-config`, even though the latter is the more
//! natural-sounding home: `liberado-config-loader`'s own cross-cutting validation
//! (`validate_merged_config`) needs this type, and `liberado-config` already depends on
//! `liberado-config-loader` — putting the model in `liberado-config` instead would create a cycle.
//! `liberado-config` re-exports everything here, so external consumers still import it as
//! `liberado_config::Config` et al. (moved from `liberado-common` 2026-07-04,
//! `docs/roadmap/hygiene-audit-2026-07-04.md`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use liberado_common::{
    Capability, CapabilitySet, Consequence, DEFAULT_POOL, Error, ModelProfile, ModelRole, Result,
    WriteClass,
};
use serde::{Deserialize, Serialize};

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
    /// Enabled external webhook hooks (Decision 6/18/19) — each is reachable at
    /// `POST /api/hooks/{name}` and dispatches `goal` through the same reactive pipeline a vault
    /// change or cron firing does.
    pub hooks: Vec<HookConfig>,
    /// Cron schedules (Decision 18/19) — each fires on its own timer and dispatches `goal` through
    /// the same reactive pipeline a vault change does (`liberado-cron`'s `CronEventSource`).
    pub schedules: Vec<CronSchedule>,
    /// Named dispatcher/executor pools (Decision 18 checkpoint #3) — each gets its own
    /// `Policy::capabilities_for(name)` authority boundary, sharing the same provider/tuning/MCP
    /// registry as everything else. The always-present `"default"` pool (today's single-dispatcher
    /// behavior) doesn't need to be declared here unless referenced for clarity; only *additional*
    /// pools need an entry so `CronSchedule.pool`/`HookConfig.pool` have something to validate
    /// against.
    pub pools: Vec<PoolConfig>,
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
            schedules: Vec::new(),
            pools: Vec::new(),
        }
    }
}

/// A named dispatcher/executor pool (Decision 18 checkpoint #3): authority segregation only, not
/// coordination — pools never communicate with each other (see
/// `docs/ideas/a2a-protocol-idea.md`'s research note on why cross-pool/agent coordination is
/// explicitly out of scope). A pool's authority is just `Policy::capabilities_for(name)` — no new
/// capability mechanism, the name *is* the component. v1 shares the same provider/tuning as every
/// other pool; only the capability grant differs (see this crate's `CronSchedule`/`HookConfig`
/// `pool` fields for how an event gets routed to one).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// A configured cron schedule: wiring only (Decision 14) — the daemon-assembly layer
/// (`liberado-bootstrap`) translates enabled entries into `liberado-cron`'s runtime `Schedule`
/// type. `cron_expr` uses the `cron` crate's 6/7-field syntax (**seconds first** — not standard
/// 5-field cron), e.g. `"0 0 9 * * * *"` for "every day at 09:00:00".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronSchedule {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub cron_expr: String,
    /// The goal text dispatched.
    pub goal: String,
    /// Which named pool (Decision 18 checkpoint #3) handles this schedule's firing — `None` routes
    /// to the always-present `"default"` pool (today's behavior, unchanged for anyone not opting
    /// in). If set, must name `"default"` or a declared, enabled `topology.pools` entry
    /// (fail-fast validated).
    #[serde(default)]
    pub pool: Option<String>,
}

/// A configured external webhook hook: wiring only (Decision 14) — `liberado-server` resolves
/// `secret_ref` from the environment and registers `POST /api/hooks/{name}` for each enabled entry.
/// `goal` mirrors [`CronSchedule::goal`]'s role exactly (cron is a *temporal* hook; this is a
/// *network-triggered* one) — the caller's optional request body only adds runtime context, it
/// never replaces the configured goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Env var holding this hook's shared secret (`X-Liberado-Hook-Secret` header) — never the
    /// secret itself (Decision 10). Each hook has its own, so leaking one doesn't compromise others.
    pub secret_ref: String,
    /// The goal text dispatched.
    pub goal: String,
    /// Which named pool (Decision 18 checkpoint #3) handles this hook's trigger — see
    /// [`CronSchedule::pool`]'s doc comment; identical semantics.
    #[serde(default)]
    pub pool: Option<String>,
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
    /// Default target zone for this MCP's write tools — a tool not named in `tools` below
    /// inherits this. `None` if this MCP has no uniform default (e.g. a general-purpose vault MCP
    /// whose tools each write to a different zone — declare each one explicitly in `tools`
    /// instead), or if none of this MCP's tools are zone-scoped writes at all. Optional, not
    /// required: an MCP that never touches vault zones (weather, a calculator) simply omits this
    /// and every one of its tools is treated as "not a zone-write concern" by
    /// `resolve_declared_zone` — zone-write-class gating is opt-in per MCP, not a blanket
    /// restrictive default the way `consequence` is (most MCPs aren't vault writers at all).
    #[serde(default)]
    pub default_zone: Option<String>,
    /// Per-tool overrides. A tool named here uses its own `zone` instead of inheriting
    /// `default_zone` — including explicitly overriding to "not a zone write" by leaving `zone`
    /// unset, for the one read tool in an otherwise all-write MCP. A tool *not* named here always
    /// inherits `default_zone` (which may itself be `None`).
    #[serde(default)]
    pub tools: Vec<ToolImpact>,
}

/// One tool's zone-write override within its owning [`McpConfig`] — see `McpConfig::default_zone`
/// and `McpConfig::tools` for when this is needed vs. when a plain `default_zone` alone suffices.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolImpact {
    /// Bare tool name (no `"<mcp>:"` prefix — implied by the owning `McpConfig`).
    pub name: String,
    /// Target zone this specific tool writes to, overriding the MCP's `default_zone`. Omit (or
    /// set to `None`) to explicitly declare this one tool as NOT a zone write, even when the
    /// MCP's other tools are.
    #[serde(default)]
    pub zone: Option<String>,
}

/// Resolve the target zone for a specific tool call, given its owning MCP's config and the tool's
/// bare name (no `"<mcp>:"` prefix). `None` means "not a zone-write concern" — a declared read, or
/// an MCP that hasn't opted into zone tracking at all — distinct from "a write whose zone is
/// unknown," which callers should treat conservatively (the zone-write-class guard's fail-safe
/// default for an unresolvable-but-real write) rather than silently skip.
///
/// Deliberately a static, per-tool *declaration* (like `consequence` already is), not per-call
/// argument introspection: a tool call's `args` aren't parsed here at all. The tradeoff this
/// accepts is real — a single generic `vault:write(path)` tool that can target any zone depending
/// on its arguments can't be discriminated by this alone; an MCP author who needs that must expose
/// distinct per-zone tool names (`vault:write_tasks`, `vault:write_reviews`, ...) instead of one
/// generic multi-zone tool, if per-zone gating actually matters for it. Chosen for simplicity and
/// consistency with the rest of the config surface over the added complexity (and MCP-argument-
/// shape coupling) of dynamic resolution; revisit only if a real MCP's shape can't be expressed
/// this way in practice.
pub fn resolve_declared_zone(mcp: &McpConfig, bare_tool_name: &str) -> Option<String> {
    match mcp.tools.iter().find(|t| t.name == bare_tool_name) {
        Some(tool) => tool.zone.clone(),
        None => mcp.default_zone.clone(),
    }
}

#[cfg(test)]
mod zone_resolution_tests {
    use super::*;

    fn mcp_with(default_zone: Option<&str>, tools: Vec<ToolImpact>) -> McpConfig {
        McpConfig {
            name: "test-mcp".into(),
            enabled: true,
            description: "test".into(),
            consequence: Consequence::Reversible,
            transport: McpTransport::Managed,
            default_zone: default_zone.map(String::from),
            tools,
        }
    }

    #[test]
    fn unlisted_tool_inherits_the_mcp_default_zone() {
        let mcp = mcp_with(Some("tasks"), Vec::new());
        assert_eq!(
            resolve_declared_zone(&mcp, "write"),
            Some("tasks".to_string())
        );
    }

    #[test]
    fn listed_tool_overrides_the_default_zone() {
        let mcp = mcp_with(
            Some("tasks"),
            vec![ToolImpact {
                name: "write_review".into(),
                zone: Some("reviews".into()),
            }],
        );
        assert_eq!(
            resolve_declared_zone(&mcp, "write_review"),
            Some("reviews".to_string())
        );
        // An unlisted tool on the same MCP still inherits the default.
        assert_eq!(
            resolve_declared_zone(&mcp, "write"),
            Some("tasks".to_string())
        );
    }

    #[test]
    fn listed_tool_with_no_zone_explicitly_overrides_to_not_a_write() {
        // Even though the MCP has a default_zone, explicitly listing a tool with no `zone`
        // declares it as NOT a zone write (e.g. the one read tool in an otherwise all-write MCP).
        let mcp = mcp_with(
            Some("tasks"),
            vec![ToolImpact {
                name: "search".into(),
                zone: None,
            }],
        );
        assert_eq!(resolve_declared_zone(&mcp, "search"), None);
    }

    #[test]
    fn no_default_zone_and_unlisted_tool_resolves_to_none() {
        // An MCP that hasn't opted into zone tracking at all -- every one of its tools is "not a
        // zone-write concern," not a fail-safe-restricted unknown.
        let mcp = mcp_with(None, Vec::new());
        assert_eq!(resolve_declared_zone(&mcp, "anything"), None);
    }
}

/// How to reach an MCP server. Stdio spawns a child process; Http connects to a URL (Decision 3);
/// Managed spawns a child process too, but at a binary path resolved by convention (see
/// [`managed_binary_path`]) instead of a literal `command` — for MCPs built and installed by
/// `liberado-mcp-forge` from a git URL, so the binary's location doesn't need hand-editing into
/// `topology.toml` every time it's rebuilt.
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
    Managed,
}

fn default_true() -> bool {
    true
}

/// Where `liberado-mcp-forge` installs a managed MCP's binary (`cargo install --root
/// <install_dir>/<name>`), and where [`McpTransport::Managed`] resolution looks for it at
/// connect-time. Single source of truth shared by both, so the two can never drift — `name` is
/// the owning [`McpConfig::name`], not a separate field, so there's nothing to keep in sync.
pub fn managed_binary_path(install_dir: &Path, name: &str) -> PathBuf {
    install_dir
        .join(name)
        .join("bin")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}

#[cfg(test)]
mod managed_binary_path_tests {
    use super::*;

    #[test]
    fn joins_install_dir_name_bin_and_platform_suffix() {
        let path = managed_binary_path(Path::new("/opt/liberado/mcp-bin"), "liberado-weather-mcp");
        let expected = PathBuf::from("/opt/liberado/mcp-bin")
            .join("liberado-weather-mcp")
            .join("bin")
            .join(format!(
                "liberado-weather-mcp{}",
                std::env::consts::EXE_SUFFIX
            ));
        assert_eq!(path, expected);
    }
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

    /// The capability set granted to `component` — the union of every [`Grant`] whose `component`
    /// matches, narrowed to just that slice of authority (this narrowing is itself the ceiling; a
    /// dispatch further narrows within it, never outside it — Decision 4). Two components are
    /// meaningful today: `"main-agent"` (the chat-facing tool surface, `ChatSessions`) and
    /// `"dispatcher"` (the ceiling the guard pipeline and `ExecuteDirect`/`DispatchSubagent`
    /// execution check against, `configure_daemon`). A grant can list either, both, or neither —
    /// an MCP granted only to `"dispatcher"` is reachable via dispatch-routed execution but never
    /// appears directly in chat.
    pub fn capabilities_for(&self, component: &str) -> CapabilitySet {
        self.grants
            .iter()
            .filter(|g| g.component == component)
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
    pub telegram_approvals: TelegramApprovalsTuning,
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
    /// Whether `ExecuteDirect`'s `relevant_mcps` narrows the executor's runtime to what the
    /// classifier judged relevant, instead of surfacing every granted MCP's full tool schemas on
    /// every turn (the token-efficiency default). `Dispatcher::dispatch` clears `relevant_mcps`
    /// to empty post-classification when this is `false`, so every downstream consumer
    /// (`Orchestrator`, `ChatSessions`) has one simple rule regardless of this setting: respect
    /// `relevant_mcps` when non-empty, else use the full grant. Flip this off to always send the
    /// full catalog, no code change required.
    pub narrow_direct_tools: bool,
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
            narrow_direct_tools: true,
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
    /// When the low-intensity whole-vault ambient sweep runs (`liberado-inbox-spec.md` §11, e.g.
    /// `"nightly"`). Free text, not yet a cron expression or any other parsed format — nothing
    /// consumes this field yet (the ambient sweep itself isn't built), so no concrete schedule
    /// syntax has been decided. `Config::validate` only rejects it being empty; whichever future
    /// component actually reads this should pick a real syntax (cron-like, most likely, matching
    /// `topology.schedules[].cron_expr`) and add proper parse validation here at that point.
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
    /// When batched git commits run (`liberado-vault-maintenance-and-git-spec.md` §5, e.g.
    /// `"per-batch+hourly"`). Free text, not yet parsed by anything — see
    /// `CaptureTuning::ambient_sweep_schedule`'s doc comment for why (same situation: the
    /// git-maintenance component that would consume this isn't built yet, so no concrete syntax is
    /// fixed). `Config::validate` only rejects it being empty.
    pub git_commit_schedule: String,
    /// Dirs Syncthing must not replicate (the `.git/` footgun + machine-managed dirs).
    pub stignore_machine_dirs: Vec<String>,
    /// When `maintenance-hook`'s hygiene sweep runs (same spec, e.g. `"weekly"`) — same
    /// not-yet-consumed, free-text situation as `git_commit_schedule` above.
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

/// `liberado-telegram-approvals`' `ApprovalBot` tunables: poll-loop timing plus the revise LLM
/// call's sampling. Broken out from the crate itself (which has no config-loader dependency of
/// its own reasoning) so these follow the same file-configurable/recompile-tolerant-default split
/// as every other tuning knob, rather than living as ungoverned inline constants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramApprovalsTuning {
    /// How long to long-poll Telegram's `getUpdates` before retrying (seconds). Telegram caps this
    /// at 50s server-side; staying under 30s also keeps the connection alive through most NAT
    /// timeouts.
    pub getupdate_timeout_secs: u64,
    /// Backoff before retrying `getUpdates` after a network/API error (seconds).
    pub poll_retry_backoff_secs: u64,
    /// Sampling temperature for the revise LLM call — 0 for a faithful, non-creative edit rather
    /// than an unrelated rewrite.
    pub revise_temperature: f32,
}

impl Default for TelegramApprovalsTuning {
    fn default() -> Self {
        Self {
            getupdate_timeout_secs: 25,
            poll_retry_backoff_secs: 10,
            revise_temperature: 0.0,
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
        // These three are free text, not yet parsed by anything (see their own doc comments) — but
        // an empty schedule is unambiguously wrong under any future interpretation, so it's caught
        // here rather than left to surface as a confusing failure once a real consumer exists.
        if self.tuning.capture.ambient_sweep_schedule.trim().is_empty() {
            return Err(Error::Config(
                "tuning.capture.ambient_sweep_schedule must not be empty".into(),
            ));
        }
        if self
            .tuning
            .maintenance
            .git_commit_schedule
            .trim()
            .is_empty()
        {
            return Err(Error::Config(
                "tuning.maintenance.git_commit_schedule must not be empty".into(),
            ));
        }
        if self
            .tuning
            .maintenance
            .maintenance_schedule
            .trim()
            .is_empty()
        {
            return Err(Error::Config(
                "tuning.maintenance.maintenance_schedule must not be empty".into(),
            ));
        }
        if self.tuning.telegram_approvals.getupdate_timeout_secs == 0
            || self.tuning.telegram_approvals.getupdate_timeout_secs > 50
        {
            return Err(Error::Config(
                "tuning.telegram_approvals.getupdate_timeout_secs must be between 1 and 50 \
                 (Telegram's own getUpdates cap)"
                    .into(),
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

        // Every schedule's cron expression must actually parse, and names must be unique — a
        // malformed or ambiguous schedule is a load-time error (Decision 14 fail-fast), not
        // something discovered only once it fails to fire.
        let mut seen_schedule_names = std::collections::HashSet::new();
        for schedule in &self.topology.schedules {
            if !seen_schedule_names.insert(&schedule.name) {
                return Err(Error::Config(format!(
                    "topology.schedules has a duplicate name '{}'",
                    schedule.name
                )));
            }
            if let Err(e) =
                std::str::FromStr::from_str(&schedule.cron_expr).map(|_: cron::Schedule| ())
            {
                return Err(Error::Config(format!(
                    "topology.schedules['{}'].cron_expr '{}' is invalid: {e}",
                    schedule.name, schedule.cron_expr
                )));
            }
        }

        // Hook names must be unique too — the env-var-existence check for each `secret_ref` is a
        // cross-cutting concern (needs the live process environment), so it lives in
        // `validate_merged_config` alongside the identical check for `policy.secret_refs`.
        let mut seen_hook_names = std::collections::HashSet::new();
        for hook in &self.topology.hooks {
            if !seen_hook_names.insert(&hook.name) {
                return Err(Error::Config(format!(
                    "topology.hooks has a duplicate name '{}'",
                    hook.name
                )));
            }
        }

        // Pool names must be unique, and any schedule/hook that names a pool must reference one
        // that actually exists (the always-present "default", or a declared, enabled entry here) —
        // fail-fast (Decision 14), not a silent typo that quietly falls back or 404s at runtime.
        let mut seen_pool_names = std::collections::HashSet::new();
        for pool in &self.topology.pools {
            if !seen_pool_names.insert(pool.name.as_str()) {
                return Err(Error::Config(format!(
                    "topology.pools has a duplicate name '{}'",
                    pool.name
                )));
            }
        }
        let pool_exists = |name: &str| {
            name == DEFAULT_POOL
                || self
                    .topology
                    .pools
                    .iter()
                    .any(|p| p.enabled && p.name == name)
        };
        for schedule in &self.topology.schedules {
            if let Some(pool) = &schedule.pool {
                if !pool_exists(pool) {
                    return Err(Error::Config(format!(
                        "topology.schedules['{}'].pool '{pool}' does not name \"default\" or a \
                         declared, enabled topology.pools entry",
                        schedule.name
                    )));
                }
            }
        }
        for hook in &self.topology.hooks {
            if let Some(pool) = &hook.pool {
                if !pool_exists(pool) {
                    return Err(Error::Config(format!(
                        "topology.hooks['{}'].pool '{pool}' does not name \"default\" or a \
                         declared, enabled topology.pools entry",
                        hook.name
                    )));
                }
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
/// use liberado_config_loader::Config;
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
    pub fn model(mut self, model: ModelProfile) -> Self {
        self.config.topology.models.push(model);
        self
    }

    /// Assign a model to a role (replaces any existing assignment for that role).
    pub fn model_role(mut self, role: ModelRole, name: impl Into<String>) -> Self {
        self.config.topology.model_roles.insert(role, name.into());
        self
    }

    /// Add an MCP server config.
    pub fn mcp(mut self, mcp: McpConfig) -> Self {
        self.config.topology.mcps.push(mcp);
        self
    }

    /// Add a hook component config.
    pub fn hook(mut self, hook: HookConfig) -> Self {
        self.config.topology.hooks.push(hook);
        self
    }

    /// Add a cron schedule.
    pub fn schedule(mut self, schedule: CronSchedule) -> Self {
        self.config.topology.schedules.push(schedule);
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
    use liberado_common::Zone;
    use liberado_common::{ModelProfile, ModelRole, ModelTier};
    use std::str::FromStr;

    #[test]
    fn capabilities_for_unions_matching_component_grants_and_dedups() {
        let policy = Policy {
            zones: Vec::new(),
            grants: vec![
                Grant {
                    component: "dispatcher".into(),
                    capabilities: vec![
                        Capability::Read(Zone::vault("tasks")),
                        Capability::Write(Zone::vault("tasks")),
                    ],
                },
                Grant {
                    component: "dispatcher".into(),
                    capabilities: vec![
                        // Overlaps with the first grant — the union must de-duplicate.
                        Capability::Read(Zone::vault("tasks")),
                        Capability::ExecuteMcp("memory-mcp".into()),
                    ],
                },
            ],
            secret_refs: Vec::new(),
        };

        let caps = policy.capabilities_for("dispatcher");
        assert!(caps.contains(&Capability::Read(Zone::vault("tasks"))));
        assert!(caps.contains(&Capability::Write(Zone::vault("tasks"))));
        assert!(caps.contains(&Capability::ExecuteMcp("memory-mcp".into())));
        // Read(tasks) appeared twice across grants but is held once.
        assert_eq!(caps.capabilities.len(), 3);
    }

    #[test]
    fn capabilities_for_excludes_grants_of_other_components() {
        let policy = Policy {
            zones: Vec::new(),
            grants: vec![
                Grant {
                    component: "main-agent".into(),
                    capabilities: vec![Capability::ExecuteMcp("weather-mcp".into())],
                },
                Grant {
                    component: "dispatcher".into(),
                    capabilities: vec![Capability::ExecuteMcp("rentcast-mcp".into())],
                },
            ],
            secret_refs: Vec::new(),
        };

        let main_agent = policy.capabilities_for("main-agent");
        assert!(main_agent.contains(&Capability::ExecuteMcp("weather-mcp".into())));
        assert!(
            !main_agent.contains(&Capability::ExecuteMcp("rentcast-mcp".into())),
            "a dispatcher-only grant must not leak into the main-agent's capability set"
        );

        let dispatcher = policy.capabilities_for("dispatcher");
        assert!(dispatcher.contains(&Capability::ExecuteMcp("rentcast-mcp".into())));
        assert!(!dispatcher.contains(&Capability::ExecuteMcp("weather-mcp".into())));
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
        assert_eq!(t.telegram_approvals.getupdate_timeout_secs, 25);
        assert_eq!(t.telegram_approvals.poll_retry_backoff_secs, 10);
        assert_eq!(t.telegram_approvals.revise_temperature, 0.0);
    }

    #[test]
    fn telegram_approvals_getupdate_timeout_must_be_within_telegrams_own_cap() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.tuning.telegram_approvals.getupdate_timeout_secs = 51;
        assert!(cfg.validate().is_err());
        cfg.tuning.telegram_approvals.getupdate_timeout_secs = 0;
        assert!(cfg.validate().is_err());
        cfg.tuning.telegram_approvals.getupdate_timeout_secs = 25;
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn empty_config_needs_a_vault_path() {
        let cfg = Config::default();
        assert!(cfg.validate().is_err(), "empty config must fail validation");
    }

    #[test]
    fn blank_schedule_fields_fail_validation() {
        let base = || {
            let mut cfg = Config::default();
            cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
            cfg
        };

        let mut cfg = base();
        cfg.tuning.capture.ambient_sweep_schedule = "  ".to_string();
        assert!(cfg.validate().is_err(), "blank ambient_sweep_schedule");

        let mut cfg = base();
        cfg.tuning.maintenance.git_commit_schedule = String::new();
        assert!(cfg.validate().is_err(), "empty git_commit_schedule");

        let mut cfg = base();
        cfg.tuning.maintenance.maintenance_schedule = String::new();
        assert!(cfg.validate().is_err(), "empty maintenance_schedule");

        assert!(base().validate().is_ok(), "defaults must still pass");
    }

    #[test]
    fn minimal_valid_config_passes() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        assert!(cfg.validate().is_ok());
    }

    fn cron_schedule(name: &str, cron_expr: &str) -> CronSchedule {
        CronSchedule {
            name: name.into(),
            enabled: true,
            cron_expr: cron_expr.into(),
            goal: "do something".into(),
            pool: None,
        }
    }

    #[test]
    fn a_valid_schedule_passes_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.schedules = vec![cron_schedule("nightly", "0 0 9 * * * *")];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn a_malformed_cron_expression_fails_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.schedules = vec![cron_schedule("nightly", "not a cron expr")];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn duplicate_schedule_names_fail_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.schedules = vec![
            cron_schedule("nightly", "0 0 9 * * * *"),
            cron_schedule("nightly", "0 0 12 * * * *"),
        ];
        assert!(cfg.validate().is_err());
    }

    fn hook_config(name: &str) -> HookConfig {
        HookConfig {
            name: name.into(),
            enabled: true,
            secret_ref: format!("{}_SECRET", name.to_uppercase()),
            goal: "do something".into(),
            pool: None,
        }
    }

    #[test]
    fn a_valid_hook_passes_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.hooks = vec![hook_config("nightly-backup")];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn duplicate_hook_names_fail_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.hooks = vec![hook_config("nightly-backup"), hook_config("nightly-backup")];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_schedule_targeting_the_implicit_default_pool_passes_with_no_pools_declared() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        let mut schedule = cron_schedule("nightly", "0 0 9 * * * *");
        schedule.pool = Some(DEFAULT_POOL.to_string());
        cfg.topology.schedules = vec![schedule];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn a_schedule_targeting_a_declared_pool_passes_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.pools = vec![PoolConfig {
            name: "restricted".into(),
            enabled: true,
        }];
        let mut schedule = cron_schedule("nightly", "0 0 9 * * * *");
        schedule.pool = Some("restricted".to_string());
        cfg.topology.schedules = vec![schedule];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn a_schedule_targeting_an_undeclared_pool_fails_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        let mut schedule = cron_schedule("nightly", "0 0 9 * * * *");
        schedule.pool = Some("nonexistent".to_string());
        cfg.topology.schedules = vec![schedule];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_hook_targeting_a_disabled_pool_fails_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.pools = vec![PoolConfig {
            name: "restricted".into(),
            enabled: false,
        }];
        let mut hook = hook_config("nightly-backup");
        hook.pool = Some("restricted".to_string());
        cfg.topology.hooks = vec![hook];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn duplicate_pool_names_fail_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.pools = vec![
            PoolConfig {
                name: "restricted".into(),
                enabled: true,
            },
            PoolConfig {
                name: "restricted".into(),
                enabled: true,
            },
        ];
        assert!(cfg.validate().is_err());
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
                default_zone: None,
                tools: Vec::new(),
            })
            .hook(HookConfig {
                name: "hook1".into(),
                enabled: true,
                secret_ref: "HOOK1_SECRET".into(),
                goal: "do something".into(),
                pool: None,
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
