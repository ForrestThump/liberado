//! Topology section: vault, providers, pools, profiles, schedules, hooks, MCPs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use liberado_common::{
    Consequence, DEFAULT_TIMEZONE, ModelProfile, ModelRole, ReasoningLevel, UserTimezone,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Topology — wiring (homelab-local). No universal Default for deployment-specific
// fields like the vault path; `validate` enforces their presence.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Topology {
    /// Path to the Obsidian vault (the source of truth). Required.
    pub vault_path: PathBuf,
    /// Operator's IANA timezone — **single source of truth** for local wall-clock across Liberado
    /// (e.g. `America/Chicago` for US Central / Texas CDT). Used when stamping "Local time: …"
    /// onto cron/webhook goals and available via [`Topology::user_timezone`] / [`UserTimezone`]
    /// anywhere a caller wants to inject now into agent context. **Not** applied to cron
    /// *expressions* (those remain UTC); only to human-facing local-time context.
    /// Default: [`DEFAULT_TIMEZONE`] (`America/Chicago`). Validated at load time.
    pub timezone: String,
    /// Unix domain socket the daemon listens on for TUI/client attach (Decision 2).
    pub daemon_socket: PathBuf,
    /// Which declared `providers` entry (by `name`) supplies inference. Provider-agnostic
    /// scaffolding (Decision 9/13) — validated against `providers` in [`Config::validate`].
    pub provider: String,
    /// Declared inference backends — base URL, default model, and env var names for each. Adding
    /// a new OpenAI-compatible backend (OpenAI direct, Groq, Together, ...) is a new entry here,
    /// not a new crate: every backend is built by the single, generic
    /// `liberado-provider-openai-compat` (`docs/roadmap/hygiene-audit-2026-07-05.md`'s follow-up).
    /// Seeded with `deepseek`/`openrouter` by default so an empty/absent config still boots exactly
    /// as before this field existed.
    pub providers: Vec<ProviderProfile>,
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
    /// Named session profiles (session-focus S6) — "run pack X wearing hat Y", where the hat is a
    /// capability grant key plus opaque pack overrides. Declaring none keeps today's behavior: a
    /// session's authority is `capabilities_for(<domain>)`.
    #[serde(default)]
    pub session_profiles: Vec<SessionProfile>,
    /// How the conversational main agent presents itself and which tools it sees.
    /// Default: human-interfacer + built-in `delegate` tool (specialist MCPs stay on the dispatcher).
    pub main_agent: MainAgentConfig,
    /// Per-role provider/model/sampling overrides (the execution-path tuning knobs). Keyed by
    /// [`ModelRole`] (`main_agent` = chat face, `dispatcher` = router, `subagent` = orchestrator/
    /// worker). Each field is optional and falls back to [`Self::provider`] + that provider's
    /// model/defaults, so an empty table is exactly today's single-model behavior. Lets the operator
    /// tier models (fast/cheap router, strong worker) and dial thinking/temperature per role from
    /// config — no rebuild. See `docs/roadmap/latency-and-routing-observability-plan.md` §3.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub roles: HashMap<ModelRole, RoleOverride>,

    /// Turn ceiling for a **read-only** subagent — research, review, summarisation. `None` →
    /// [`liberado_orchestrator::RESEARCH_MAX_TURNS`].
    ///
    /// Gathering work is turn-hungry in a way acting work is not: a live deep-research run spent
    /// all 8 of the general subagent turns on ~28 searches and never reached its write-up. A
    /// read-only run cannot leave anything half-changed, so the only cost of a high ceiling is
    /// tokens. Tunable here rather than compiled in, like the per-role model settings above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_max_turns: Option<u32>,

    /// Which MCP tool writes a `Delivery::Vault` report. `None` → vault delivery is unavailable and
    /// every report is summarized by the main agent (exactly the behaviour before this existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_sink: Option<ReportSinkConfig>,
}

/// The one tool call that lands a `Delivery::Vault` report.
///
/// Declared rather than inferred. The orchestrator is kernel-layer and cannot depend on
/// `liberado-vault`, so it reaches the vault the same way everything else does — through an MCP
/// tool — and it therefore has to be told *which* tool, and what the argument is called. Guessing
/// `write_note(path, content)` would work for TurboVault today and fail silently the moment the
/// vault MCP is swapped, which is the failure mode Decision 14 exists to prevent: a declared,
/// boot-validated sink turns "the report vanished" into "the daemon refused to start".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportSinkConfig {
    /// The `[[mcps]]` entry (by `name`) that owns the write tool. Must exist, and must not be
    /// `read_only` — validated at load.
    pub mcp: String,
    /// Bare tool name on that MCP (e.g. `"write_note"`). When the MCP declares `write_tools`, this
    /// must be one of them — otherwise the "sink" would be a read, and the report would go nowhere
    /// while reporting success.
    pub tool: String,
    /// Argument carrying the destination path. Defaults to `"path"`.
    #[serde(default = "default_path_arg")]
    pub path_arg: String,
    /// Argument carrying the report body. Defaults to `"content"`.
    #[serde(default = "default_content_arg")]
    pub content_arg: String,
}

fn default_path_arg() -> String {
    "path".to_string()
}

fn default_content_arg() -> String {
    "content".to_string()
}

/// Per-role overrides for the execution path. All fields optional; unset = inherit the global
/// provider + its model, and leave sampling to the per-call defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoleOverride {
    /// Which declared `[[topology.providers]]` entry (by `name`) serves this role. `None` → the
    /// global [`Topology::provider`]. Validated against `providers` in [`Config::validate`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model slug to send for this role (e.g. `"deepseek/deepseek-v3-flash"`). `None` → the
    /// provider profile's env/default model. Free-form (any slug the backend accepts), like the
    /// `*_MODEL` env vars — not required to be a declared `[[topology.models]]` entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Sampling temperature for this role. When set, it **overrides** the per-call temperature
    /// (e.g. the dispatcher's pinned 0). `None` → leave per-call behavior unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Reasoning ("thinking") level for this role. `None` → provider/model default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLevel>,
}

/// Chat main-agent surface: human interface first, optional extra MCP tools via `policy.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MainAgentConfig {
    /// When `true` (default), the main agent is a **human interfacer**: it gets the built-in
    /// `delegate` tool (dispatcher/orchestrator behind the scenes) instead of a pre-turn dispatch
    /// that injects the full MCP fleet into chat context. Specialist tools should be granted to
    /// the `"dispatcher"` component in `policy.toml`, not `"main-agent"`.
    ///
    /// When `false`, legacy behavior: pre-turn dispatch + all `"main-agent"` ExecuteMcp tools
    /// surfaced on the streaming path.
    pub delegation_mode: bool,
    /// Optional full override of the main-agent system prompt. When unset, uses the built-in
    /// human-interfacer prompt (if `delegation_mode`) or the short legacy prompt otherwise.
    pub system_prompt: Option<String>,
    /// Automatic context compaction for long conversations (CH3 — see
    /// `docs/roadmap/context-compaction-plan.md`). All fields defaulted; an absent table is the
    /// shipped behavior (compaction on).
    pub compaction: CompactionSettings,
}

/// Absolute estimated-token trigger used when no model window / percentage can be resolved
/// (no matching `[[models]]` entry, or empty models list). Matches the historical CH3 default
/// (≈ 64k window − 16k reserve).
pub const COMPACTION_TRIGGER_TOKENS_FALLBACK: u32 = 48_000;

/// Default fraction of a model's declared [`ModelProfile::context_window`] at which compaction
/// fires when no absolute `trigger_tokens` override is set (0.75 ≈ 48k on a 64k window).
pub const COMPACTION_TRIGGER_PCT_DEFAULT: f32 = 0.75;

/// Automatic context-compaction knobs (`[main_agent.compaction]`). The server resolves an effective
/// absolute `trigger_tokens` for the **face** model via
/// [`CompactionSettings::resolve_trigger_tokens`], then mirrors the rest into
/// `liberado_main_agent::CompactionConfig`.
///
/// Trigger resolution (first match wins):
/// 1. Per-model absolute: `[main_agent.compaction.models."<name>"].trigger_tokens`
/// 2. Per-model pct × that model's `[[models]].context_window`
/// 3. Global absolute: `[main_agent.compaction].trigger_tokens` (when set)
/// 4. Global `trigger_pct` × face model's `context_window`
/// 5. [`COMPACTION_TRIGGER_TOKENS_FALLBACK`] when the face model has no declared window
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionSettings {
    /// Master switch, default ON — a reliability guard that is opt-in is off in practice.
    pub enabled: bool,
    /// Default fraction of the face model's declared `context_window` at which compaction fires.
    /// Overridden by absolute `trigger_tokens` (global or per-model) when those are set.
    /// Clamped to `[0.0, 1.0]` at resolve time. Default [`COMPACTION_TRIGGER_PCT_DEFAULT`].
    pub trigger_pct: f32,
    /// Global absolute trigger in estimated tokens (chars/4 × 1.3). When set, overrides
    /// `trigger_pct` for any model without a more specific per-model absolute. When unset,
    /// percentage-of-window is used for models that declare `context_window`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_tokens: Option<u32>,
    /// Per-model trigger overrides, keyed by model name (same string as `[[models]].name` and/or
    /// the live face provider model slug). Absolute tokens win over pct for that model.
    #[serde(default)]
    pub models: HashMap<String, ModelCompactionSettings>,
    /// User turns kept verbatim after the summary (boundary anchored on user messages so
    /// tool-call/result pairs never split). 0 = keep nothing but the summary.
    pub keep_recent_turns: u32,
    /// Hard cap on the rolling summary's own length.
    pub summary_max_tokens: u32,
    /// Per-tool-result truncation in the transcript shown to the summarizer.
    pub tool_result_max_chars: u32,
}

/// Per-model compaction trigger overrides under `[main_agent.compaction.models."<name>"]`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCompactionSettings {
    /// Fraction of this model's `context_window`. Ignored when `trigger_tokens` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_pct: Option<f32>,
    /// Absolute estimated-token trigger for this model only. Wins over any percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_tokens: Option<u32>,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger_pct: COMPACTION_TRIGGER_PCT_DEFAULT,
            trigger_tokens: None,
            models: HashMap::new(),
            keep_recent_turns: 3,
            summary_max_tokens: 1_024,
            tool_result_max_chars: 2_000,
        }
    }
}

/// How [`CompactionSettings::resolve_trigger_tokens`] obtained the absolute threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTriggerSource {
    PerModelAbsolute,
    PerModelPct,
    GlobalAbsolute,
    GlobalPct,
    /// Hard 48k path — face model had no matching `[[models]]` window and no absolute override.
    Fallback,
}

impl CompactionSettings {
    /// Resolve the absolute estimated-token trigger for the chat face model.
    ///
    /// `face_model` is the live provider model slug (e.g. from `Provider::model()`). `models` is
    /// `topology.models` — used for `context_window` when applying a percentage.
    ///
    /// When `models` is non-empty and resolution hits [`CompactionTriggerSource::Fallback`]
    /// (slug mismatch / missing profile), emits a `tracing::warn` so operators notice that
    /// declared windows were not used.
    pub fn resolve_trigger_tokens(&self, face_model: Option<&str>, models: &[ModelProfile]) -> u32 {
        let (tokens, source) = self.resolve_trigger_tokens_with_source(face_model, models);
        if source == CompactionTriggerSource::Fallback && !models.is_empty() && face_model.is_some()
        {
            tracing::warn!(
                face_model = face_model.unwrap_or(""),
                models_configured = models.len(),
                fallback = COMPACTION_TRIGGER_TOKENS_FALLBACK,
                "face model has no matching [[models]] entry (exact name); compaction trigger \
                 using hard fallback — declare the slug under [[models]] or set trigger_tokens"
            );
        }
        tokens
    }

    /// Same as [`Self::resolve_trigger_tokens`] but returns how the value was chosen (for tests
    /// and callers that need to distinguish fallback from an intentional absolute).
    pub fn resolve_trigger_tokens_with_source(
        &self,
        face_model: Option<&str>,
        models: &[ModelProfile],
    ) -> (u32, CompactionTriggerSource) {
        let profile = face_model.and_then(|name| models.iter().find(|m| m.name == name));
        let per = face_model.and_then(|name| self.models.get(name));

        // 1. Per-model absolute
        if let Some(t) = per.and_then(|p| p.trigger_tokens) {
            return (t, CompactionTriggerSource::PerModelAbsolute);
        }
        // 2. Per-model pct × window
        if let (Some(pct), Some(p)) = (per.and_then(|m| m.trigger_pct), profile) {
            return (
                pct_of_window(p.context_window, pct),
                CompactionTriggerSource::PerModelPct,
            );
        }
        // 3. Global absolute
        if let Some(t) = self.trigger_tokens {
            return (t, CompactionTriggerSource::GlobalAbsolute);
        }
        // 4. Global pct × window
        if let Some(p) = profile {
            return (
                pct_of_window(p.context_window, self.trigger_pct),
                CompactionTriggerSource::GlobalPct,
            );
        }
        // 5. Hard fallback
        (
            COMPACTION_TRIGGER_TOKENS_FALLBACK,
            CompactionTriggerSource::Fallback,
        )
    }
}

fn pct_of_window(context_window: u32, pct: f32) -> u32 {
    let pct = pct.clamp(0.0, 1.0);
    let n = (context_window as f64 * f64::from(pct)).floor() as u32;
    // A zero window or zero pct would fire every turn; keep a one-token floor so the math is
    // defined. Operators who want "always compact" can set `trigger_tokens = 1`.
    n.max(1)
}

impl Default for MainAgentConfig {
    fn default() -> Self {
        Self {
            delegation_mode: true,
            system_prompt: None,
            compaction: CompactionSettings::default(),
        }
    }
}

impl Default for Topology {
    fn default() -> Self {
        Self {
            research_max_turns: None,
            report_sink: None,
            vault_path: PathBuf::new(),
            timezone: DEFAULT_TIMEZONE.to_string(),
            daemon_socket: PathBuf::from("/run/liberado/daemon.sock"),
            provider: "deepseek".to_string(),
            providers: default_providers(),
            models: Vec::new(),
            model_roles: HashMap::new(),
            mcps: Vec::new(),
            hooks: Vec::new(),
            schedules: Vec::new(),
            pools: Vec::new(),
            session_profiles: Vec::new(),
            main_agent: MainAgentConfig::default(),
            roles: HashMap::new(),
        }
    }
}

impl Topology {
    /// Resolve [`Self::timezone`] to a validated [`UserTimezone`].
    ///
    /// Prefer this (or the clock on the running daemon) over re-parsing the string at call sites.
    /// Load-time [`Config::validate`] already rejects unknown names, so in a booted daemon this
    /// is infallible unless the string was mutated after load.
    pub fn user_timezone(
        &self,
    ) -> std::result::Result<UserTimezone, liberado_common::UnknownTimezone> {
        UserTimezone::parse(&self.timezone)
    }
}

/// The two backends this system has always shipped with, as literal defaults — deliberately
/// plain string literals here rather than `liberado_provider_openai_compat::OpenAiCompatibleProvider`'s
/// constants: this crate must not depend on a concrete provider crate (that would invert the
/// intended layering, config is foundational, providers are not).
fn default_providers() -> Vec<ProviderProfile> {
    vec![
        ProviderProfile {
            name: "deepseek".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            default_model: "deepseek-chat".to_string(),
            api_key_env: "DEEPSEEK_API_KEY".to_string(),
            model_env: Some("DEEPSEEK_MODEL".to_string()),
            extra_client_error_status: Vec::new(),
        },
        ProviderProfile {
            name: "openrouter".to_string(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            default_model: "openai/gpt-4o-mini".to_string(),
            api_key_env: "OPENROUTER_API_KEY".to_string(),
            model_env: Some("OPENROUTER_MODEL".to_string()),
            extra_client_error_status: vec![402],
        },
    ]
}

/// One declared inference backend — everything `liberado-provider-openai-compat`'s generic
/// `OpenAiCompatibleProvider::from_env` needs to construct a provider for it. Adding a backend
/// this system has never shipped with (OpenAI direct, Groq, Together, ...) is one more entry here,
/// not a new Rust crate — see [`Topology::providers`]'s own doc comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderProfile {
    /// Matched against `topology.provider` to select this entry.
    pub name: String,
    pub base_url: String,
    /// Used when `model_env` is absent, or set but not present in the environment.
    pub default_model: String,
    /// Env var holding the API key.
    pub api_key_env: String,
    /// Env var that overrides `default_model` when present — `None` if this backend has no such
    /// override convention.
    #[serde(default)]
    pub model_env: Option<String>,
    /// Status codes beyond the common OpenAI-compatible set this backend's API treats as a client
    /// error rather than a generic transport failure (e.g. OpenRouter's `402` for insufficient
    /// account credits).
    #[serde(default)]
    pub extra_client_error_status: Vec<u16>,
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

/// A named **session profile** (session-focus S6) — "run this pack wearing this hat".
///
/// A profile is the goal-session analogue of a [`PoolConfig`]: authority segregation plus a little
/// pack-local flavor. It answers three questions and nothing else:
///
/// * `domain` — which registered domain pack runs the session (`"life"`, `"coding"`, …).
/// * `component` — the capability grant key. Like a pool, the **name is the component**: the
///   session's authority is exactly `Policy::capabilities_for(component)`, no new mechanism. This
///   is what lets a `research` profile on the life pack hold strictly less than the default one —
///   including omitting [`Capability::AskHuman`](liberado_common::Capability::AskHuman), which
///   makes the session structurally unable to interrupt a human.
/// * `overrides` — an **opaque** blob the pack parses itself (role, model, prompt path, …). The
///   config stack deliberately does not interpret it, exactly like `[tuning.coder]`: adding a knob
///   to a pack must never require a change here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionProfile {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Which domain pack runs sessions started under this profile.
    pub domain: String,
    /// Capability grant key — `policy.toml`'s `[[grants]] component = "…"`. Defaults to `name`
    /// (the pool rule: the name *is* the component) when omitted.
    #[serde(default)]
    pub component: Option<String>,
    /// Kernel idle budget for interactive sessions under this profile (E5): how long the hub waits
    /// on human input before `BudgetExhausted`. `None` = wait indefinitely (or the per-goal
    /// `GoalSpec.max_idle_secs` wins when set). Interactive coding profiles typically want hours.
    #[serde(default)]
    pub max_idle_secs: Option<u64>,
    /// Opaque, pack-parsed overrides. Never interpreted by the config stack.
    #[serde(default = "empty_table")]
    pub overrides: toml::Value,
}

/// An empty TOML table — `toml::Value` has no `Default`, and "no overrides" must deserialize to an
/// empty table (not a null) so packs can parse it uniformly.
pub(crate) fn empty_table() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

impl SessionProfile {
    /// The capability grant key this profile resolves to — `component` when set, else `name`.
    pub fn component_key(&self) -> &str {
        self.component.as_deref().unwrap_or(&self.name)
    }
}

/// A configured cron schedule: wiring only (Decision 14) — the daemon-assembly layer
/// (`liberado-bootstrap`) translates enabled entries into `liberado-cron`'s runtime `Schedule`
/// type. `cron_expr` uses the `cron` crate's 6/7-field syntax (**seconds first** — not standard
/// 5-field cron), e.g. `"0 0 9 * * * *"` for "every day at 09:00:00" **UTC**. Local wall-clock for
/// the model is separate: set `topology.timezone` once; the daemon stamps "Local time: …" onto the
/// goal text when a schedule fires (see [`liberado_common::UserTimezone`]).
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
    /// Optional `[[session_profiles]]` hat for this schedule (E7). When set, the reaction session
    /// resolves its grant (and idle budget) from the profile — so a cron that *wants* `AskHuman`
    /// can opt in; crons without a profile keep the pool grant, which should omit `AskHuman` (D-d).
    #[serde(default)]
    pub profile: Option<String>,
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
    /// Default target zone for this MCP's write tools — a tool not named in `tools` below inherits
    /// this. Use it for a **fixed-zone** MCP (every tool lands in the same place).
    ///
    /// Zone declaration used to be *opt-in*, and this comment used to say so approvingly. That was
    /// the bug (F1): opting in was the safe choice, nobody took it, and so `resolve_zone` returned
    /// `None` for every tool of every MCP — leaving both the capability guard and the
    /// zone-write-class guard permanently inert. A guard that is off by default is not a guard.
    ///
    /// A non-`read_only` MCP must now declare one of `default_zone`, `zone_from_arg` +
    /// `write_tools`, or `writes_vault = false`, and validation refuses to boot otherwise.
    #[serde(default)]
    pub default_zone: Option<String>,
    /// Per-tool overrides. A tool named here uses its own `zone` instead of inheriting
    /// `default_zone` — including explicitly overriding to "not a zone write" by leaving `zone`
    /// unset, for the one read tool in an otherwise all-write MCP. A tool *not* named here always
    /// inherits `default_zone` (which may itself be `None`).
    #[serde(default)]
    pub tools: Vec<ToolImpact>,
    /// **Path-addressed MCPs only** (TurboVault): the argument whose leading path segment names the
    /// zone this call writes to — e.g. `zone_from_arg = "path"`, so `write_note(path =
    /// "decisions/x.md")` resolves to zone `decisions`.
    ///
    /// A fixed `default_zone` cannot describe such an MCP: one `write_note` can land in any zone,
    /// so declaring a single zone would authorize writes to *every* zone under one capability.
    #[serde(default)]
    pub zone_from_arg: Option<String>,
    /// **Path-addressed MCPs only**: which of this MCP's tools actually write. Everything not named
    /// here is a read. Required alongside `zone_from_arg`, because a path argument alone cannot
    /// tell `read_note` from `write_note` — both have one.
    #[serde(default)]
    pub write_tools: Vec<String>,
    /// Set `false` to declare "this MCP has effects, but none of them are **vault zone** writes" —
    /// a PDF tool that writes files, a memory MCP that writes its own store.
    ///
    /// This exists so the opt-out is a **statement**, not a silence. An MCP that simply said nothing
    /// about zones is what F1 was: the guard resolved no zone, so it never fired, and nobody
    /// noticed for months. A non-`read_only` MCP must now either say where its vault writes land or
    /// say that it makes none — and validation refuses to boot until it does.
    ///
    /// Trust boundary, stated plainly: an MCP that declares `writes_vault = false` and then writes
    /// the vault anyway defeats this. That is a human asserting something false in config, which is
    /// a different (and much more visible) problem from a default that quietly protects nothing.
    #[serde(default)]
    pub writes_vault: Option<bool>,
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
            zone_from_arg: None,
            write_tools: Vec::new(),
            writes_vault: None,
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
    /// Runs the MCP server inside a container instead of directly as a host child process —
    /// isolation for a less-trusted or freshly-scaffolded MCP (e.g. one `riggers` just produced,
    /// not yet human-reviewed). Reuses the exact same `StdioConnector`/`ChildProcessTransport`
    /// machinery as [`McpTransport::Stdio`]: MCP-over-stdio doesn't care whether the child process
    /// is a bare binary or `docker run -i --rm image ...`, both are just a piped stdin/stdout
    /// process. `command: None` means "use the image's own `CMD`/`ENTRYPOINT`."
    Docker {
        image: String,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        /// Docker CLI format: `"host:container"` or `"host:container:ro"`. Host paths need
        /// forward slashes even on Windows (Docker Desktop's WSL2 backend requirement).
        #[serde(default)]
        volumes: Vec<String>,
        /// Docker CLI format: `"KEY=value"`, or a bare `"KEY"` to pass its value through from the
        /// host's own environment — the way to reach a container without a secret ever touching
        /// `topology.toml` (Decision 10).
        #[serde(default)]
        env: Vec<String>,
    },
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

#[cfg(test)]
mod compaction_trigger_resolve_tests {
    use super::*;
    use liberado_common::ModelTier;

    fn model(name: &str, window: u32) -> ModelProfile {
        ModelProfile {
            name: name.into(),
            tool_calling: true,
            structured_output: false,
            context_window: window,
            tier: ModelTier::WorkPlane,
            cost: None,
        }
    }

    #[test]
    fn fallback_when_no_model_and_no_absolute() {
        let c = CompactionSettings::default();
        assert_eq!(
            c.resolve_trigger_tokens(Some("unknown"), &[]),
            COMPACTION_TRIGGER_TOKENS_FALLBACK
        );
        assert_eq!(
            c.resolve_trigger_tokens(None, &[]),
            COMPACTION_TRIGGER_TOKENS_FALLBACK
        );
    }

    #[test]
    fn global_pct_times_declared_window() {
        let c = CompactionSettings {
            trigger_pct: 0.75,
            ..CompactionSettings::default()
        };
        let models = vec![model("deepseek-chat", 64_000)];
        assert_eq!(
            c.resolve_trigger_tokens(Some("deepseek-chat"), &models),
            48_000
        );
    }

    #[test]
    fn global_absolute_overrides_pct() {
        let c = CompactionSettings {
            trigger_pct: 0.75,
            trigger_tokens: Some(12_000),
            ..CompactionSettings::default()
        };
        let models = vec![model("deepseek-chat", 64_000)];
        assert_eq!(
            c.resolve_trigger_tokens(Some("deepseek-chat"), &models),
            12_000
        );
    }

    #[test]
    fn per_model_pct_overrides_global_pct() {
        let mut c = CompactionSettings {
            trigger_pct: 0.75,
            ..CompactionSettings::default()
        };
        c.models.insert(
            "big".into(),
            ModelCompactionSettings {
                trigger_pct: Some(0.5),
                trigger_tokens: None,
            },
        );
        let models = vec![model("big", 128_000), model("small", 32_000)];
        assert_eq!(c.resolve_trigger_tokens(Some("big"), &models), 64_000);
        // Unlisted model still uses global pct.
        assert_eq!(c.resolve_trigger_tokens(Some("small"), &models), 24_000);
    }

    #[test]
    fn per_model_absolute_wins_over_everything() {
        let mut c = CompactionSettings {
            trigger_pct: 0.9,
            trigger_tokens: Some(99_000),
            ..CompactionSettings::default()
        };
        c.models.insert(
            "face".into(),
            ModelCompactionSettings {
                trigger_pct: Some(0.1),
                trigger_tokens: Some(7_777),
            },
        );
        let models = vec![model("face", 200_000)];
        assert_eq!(c.resolve_trigger_tokens(Some("face"), &models), 7_777);
    }

    #[test]
    fn different_models_resolve_independently() {
        let mut c = CompactionSettings::default();
        c.models.insert(
            "a".into(),
            ModelCompactionSettings {
                trigger_tokens: Some(10_000),
                trigger_pct: None,
            },
        );
        c.models.insert(
            "b".into(),
            ModelCompactionSettings {
                trigger_pct: Some(0.5),
                trigger_tokens: None,
            },
        );
        let models = vec![model("a", 64_000), model("b", 100_000)];
        assert_eq!(c.resolve_trigger_tokens(Some("a"), &models), 10_000);
        assert_eq!(c.resolve_trigger_tokens(Some("b"), &models), 50_000);
    }

    #[test]
    fn toml_round_trip_per_model_table() {
        let raw = r#"
enabled = true
trigger_pct = 0.8
trigger_tokens = 50000

[models."deepseek-chat"]
trigger_pct = 0.7

[models."openai/gpt-4o"]
trigger_tokens = 100000
"#;
        let c: CompactionSettings = toml::from_str(raw).expect("parse");
        assert!((c.trigger_pct - 0.8).abs() < f32::EPSILON);
        assert_eq!(c.trigger_tokens, Some(50_000));
        assert_eq!(
            c.models.get("deepseek-chat").and_then(|m| m.trigger_pct),
            Some(0.7)
        );
        assert_eq!(
            c.models.get("openai/gpt-4o").and_then(|m| m.trigger_tokens),
            Some(100_000)
        );
    }

    #[test]
    fn slug_mismatch_with_configured_models_is_fallback_source() {
        let c = CompactionSettings::default();
        let models = vec![model("deepseek-chat", 64_000)];
        // Live provider often returns a prefixed slug that does not equal [[models]].name.
        let (tokens, source) =
            c.resolve_trigger_tokens_with_source(Some("deepseek/deepseek-chat"), &models);
        assert_eq!(tokens, COMPACTION_TRIGGER_TOKENS_FALLBACK);
        assert_eq!(source, CompactionTriggerSource::Fallback);
    }

    #[test]
    fn matching_slug_is_global_pct_not_fallback() {
        let c = CompactionSettings {
            trigger_pct: 0.75,
            ..CompactionSettings::default()
        };
        let models = vec![model("deepseek-chat", 64_000)];
        let (tokens, source) = c.resolve_trigger_tokens_with_source(Some("deepseek-chat"), &models);
        assert_eq!(tokens, 48_000);
        assert_eq!(source, CompactionTriggerSource::GlobalPct);
    }
}
