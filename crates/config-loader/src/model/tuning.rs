//! Tuning section: dispatch, context, concurrency, capture, maintenance, surfaces.

use serde::{Deserialize, Serialize};

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
    pub cron_delivery: CronDeliveryTuning,
    /// MCP connection pooling (M1) — reuse healthy peer connections across executions.
    pub mcp_pooling: McpPoolingTuning,
    /// Proposal lifecycle: expiry reaper interval, etc.
    pub proposals: ProposalTuning,
    /// The `[tuning.coder]` section, kept **opaque** here — the coding pack owns its own config
    /// vocabulary (`liberado_coder_core::CoderTuning::from_value` parses + validates it at
    /// composition time). The config stack stays pack-agnostic: it knows a pack section exists,
    /// not its shape. When a second domain pack grows a section, generalize this to a map of
    /// pack-name → raw value rather than adding more typed fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coder: Option<toml::Value>,
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
            max_concurrent_coding_subagents: 3,
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
///
/// # PARTIALLY IMPLEMENTED
///
/// Live: `inbox_ignore_globs` (watcher denylist), `inbox_path` (the watcher's capture folder),
/// `capture_paths` (extra whitelist entries — pinned file, extra folders, globs), `ready_flag`
/// (`#now` promotion), and `hold_flag` (`#hold-off` parking) — the F12 positive scope. No other
/// field on this struct is read by production code yet: there are no settle windows and no
/// ambient sweep.
///
/// The vault watcher itself *is* live and does fire; what is missing is the inbox layer above it
/// (E2) — the settle windows, the `processed/` move, and the ambient sweep.
///
/// Kept rather than deleted because the spec it implements is still the intended design and the
/// shape is agreed. `Config::validate` warns when an operator sets unimplemented fields, so config
/// that looks live but is inert says so out loud instead of being discovered on a running box.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Extra whitelist entries beyond [`inbox_path`](Self::inbox_path): pinned widget files,
    /// additional folders, or globs (`*.md`). Empty by default — the watcher still scopes to
    /// `inbox_path`. An empty list is *not* "react to everything".
    pub capture_paths: Vec<String>,
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
            capture_paths: Vec::new(),
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

/// Timing for folding a finished cron brief into the sticky Telegram conversation, deferred around
/// the human's activity so a brief never barges into an active chat — see
/// `docs/future-work/ideas/cron-delivery-timing-idea.md`. The cron itself still runs on time; only the delivery
/// to the phone defers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CronDeliveryTuning {
    /// Deliver once the Telegram chat has gone this long with no inbound (human) message. The common
    /// case — a brief firing while you are not chatting — delivers immediately, because the chat is
    /// already idle past this window; the delay only bites when you are mid-conversation.
    pub quiet_delay_secs: u64,
    /// Hard cap on how long a brief may be held waiting for quiet. Once this elapses since the brief
    /// became ready, deliver it anyway (even into an active chat), so it is never indefinitely late.
    pub deliver_by_secs: u64,
}

impl Default for CronDeliveryTuning {
    fn default() -> Self {
        Self {
            quiet_delay_secs: 300, // 5 minutes idle → deliver
            deliver_by_secs: 2700, // 45 minutes → deliver regardless
        }
    }
}

/// MCP connection pooling (`liberado-mcp` M1). Defaults **on** — life-ops runs many short
/// sessions, and a fresh handshake per execution was the primary unattended latency tax.
///
/// Set `enabled = false` to restore connect-per-acquisition (useful for debugging flaky peers).
/// `idle_ttl_secs` is enforced both on the next acquire **and** by a background reaper so idle
/// child processes / HTTP sessions do not pin forever when a peer is never re-acquired.
/// `max_in_flight_per_name` caps concurrent checkouts/connects for one MCP (parallel goals).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpPoolingTuning {
    /// When `true` (default), `McpRegistry` reuses healthy connections across
    /// `RuntimeFactory::runtime_for` acquisitions. When `false`, every acquisition connects fresh.
    pub enabled: bool,
    /// Idle time after which a returned pooled connection is discarded (eagerly on pool activity
    /// and by the background reaper). Default 300s (5 minutes).
    pub idle_ttl_secs: u64,
    /// Max simultaneous live checkouts/connects for one MCP name. Default 4.
    pub max_in_flight_per_name: usize,
    /// How long an acquire waits for a concurrency permit before failing. Default 60s.
    pub connect_wait_secs: u64,
}

impl Default for McpPoolingTuning {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_ttl_secs: 300,
            max_in_flight_per_name: 4,
            connect_wait_secs: 60,
        }
    }
}

/// Proposal lifecycle tunables: expiry reaper stroke interval, etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalTuning {
    /// How often (seconds) the background reaper sweeps `proposals/` for expired proposals and
    /// flips them to `status: expired`. 0 disables the reaper entirely.
    pub reap_interval_secs: u64,
}

impl Default for ProposalTuning {
    fn default() -> Self {
        Self {
            reap_interval_secs: 600,
        }
    }
}
