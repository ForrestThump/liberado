//! # liberado-bootstrap
//!
//! The **assembly** half of daemon composition: turns a loaded config (plus the env-sourced
//! provider) into a wired daemon — the dispatcher's capabilities + catalog and the MCP runtime
//! registry all come from `policy`/`topology`, while the inference provider still comes from the
//! environment (whichever API key `config.topology.provider` selects — see [`provider_from_config`]).
//! Keeping daemon assembly in one place means the `cli` and server binaries build the same daemon
//! the same way, so the modes (watch-only / decide-only / act) can't drift apart between them.
//!
//! The config **loader** itself (Decision 14 — resolve + merge the small per-section TOML files into
//! one validated `Config`) and the light path-resolution helpers built on it (`config_dir`,
//! `mcp_install_dir`, `data_dir`, `GuardContext`) live in `liberado-config` instead — this crate's
//! heavy assembly functions need `liberado-daemon`/`liberado-mcp`/`liberado-dispatcher`/
//! `liberado-orchestrator`/`liberado-provider-openai-compat`, which a config-only consumer
//! (`liberado-mcp-forge`) has no use for. Re-exported here so `liberado-server`/`liberado-cli` see no
//! change from before this split.

pub use liberado_config::{
    Config, ConfigError, ConfigProvenance, GuardContext, capability_catalog_from_config,
    catalog_from_config, config_dir, data_dir, guard_context, load_config, mcp_install_dir,
    sessions_dir,
};

mod mcp_apply;
pub use mcp_apply::{LiveMcpController, McpApplyError, McpApplyReport, apply_mcp_peer_set};

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use liberado_common::{
    CapabilityCatalog, CapabilitySet, DEFAULT_POOL, ModelRole, ToolGuidanceSource,
};
use liberado_config::{ProviderProfile, RoleOverride};
use liberado_daemon::Daemon;
use liberado_dispatch_pack::DispatchPack;
use liberado_dispatcher::Dispatcher;
use liberado_mcp::{McpPoolSettings, McpRegistry};
use liberado_notify::Notifier;
use liberado_orchestrator::{Orchestrator, OrchestratorInfra, ReportSink};
use liberado_provider::{AgentRole, LatencyRecorder, MeteredProvider, Provider};
use liberado_provider_openai_compat::OpenAiCompatibleProvider;

/// Build the shared inference provider from the environment, selecting the backend named by
/// `config.topology.provider` against the declared `config.topology.providers` table
/// (`ProviderProfile`s — `crates/config-loader/src/model.rs`). Adding a new OpenAI-compatible
/// backend (OpenAI direct, Groq, Together, ...) is a `[[topology.providers]]` config entry, not a
/// new Rust type — `Config::validate` already guarantees `config.topology.provider` names a
/// declared entry, so the lookup here can't silently miss (an "unknown provider" case would mean
/// the config was hand-edited after validation, which this still degrades safely from). `None`
/// means the selected backend's API key isn't set, so the daemon runs watch-only and chat is
/// disabled.
pub fn provider_from_config(config: &Config) -> Option<Arc<dyn Provider>> {
    let profile = resolve_provider_profile(config, &config.topology.provider)?;
    match build_provider_from_profile(profile, None) {
        Some(provider) => {
            tracing::info!(
                model = provider.model(),
                provider = %profile.name,
                "provider configured"
            );
            Some(provider)
        }
        None => None,
    }
}

/// Find the declared `[[topology.providers]]` entry named `name`, falling back to `deepseek` (with a
/// warning) if it isn't declared — the same defensive resolution `provider_from_config` has always
/// used, factored out so per-role provider selection resolves identically.
fn resolve_provider_profile<'a>(config: &'a Config, name: &str) -> Option<&'a ProviderProfile> {
    config
        .topology
        .providers
        .iter()
        .find(|p| p.name == name)
        .or_else(|| {
            tracing::warn!(
                provider = name,
                "provider names no declared topology.providers entry — falling back to deepseek"
            );
            config
                .topology
                .providers
                .iter()
                .find(|p| p.name == "deepseek")
        })
}

/// Build a provider for `profile`, applying a per-role override (model slug + sampling) when given.
/// `None` when the profile's API key isn't set in the environment.
fn build_provider_from_profile(
    profile: &ProviderProfile,
    role_override: Option<&RoleOverride>,
) -> Option<Arc<dyn Provider>> {
    let provider = OpenAiCompatibleProvider::from_env(
        &profile.api_key_env,
        profile.model_env.as_deref(),
        &profile.default_model,
        &profile.base_url,
        profile.extra_client_error_status.clone(),
    )
    .ok()?;

    // Apply the role's model + sampling overrides (all optional; unset = provider default).
    let provider = if let Some(ov) = role_override {
        if let Some(model) = &ov.model {
            provider.set_model(model.clone());
        }
        provider
            .with_temperature(ov.temperature)
            .with_reasoning_effort(ov.reasoning.map(|r| r.as_str().to_string()))
    } else {
        provider
    };

    Some(Arc::new(provider))
}

/// A `CoderProviderFactory` that honours the model each coding role asks for.
///
/// The pack's own `SingleProviderFactory` returns the one daemon provider for every role,
/// whatever `CoderRoleConfig::model` says — so `[coder.coder].model` selected nothing, and the
/// session event log reported the placeholder `"session-coder"` as the model in use.
///
/// A fresh provider per call, deliberately. `Provider::set_model` writes through a `RwLock` on the
/// shared trait object, so re-modelling the daemon's provider would change the model for every
/// other holder — the chat face agent included.
pub struct CoderRoleProviderFactory {
    profile: ProviderProfile,
}

impl CoderRoleProviderFactory {
    /// `None` when the configured provider has no profile or its API key is unset, so the caller
    /// keeps whatever provider it already had rather than silently losing coding.
    pub fn for_config(config: &Config) -> Option<Self> {
        let profile = resolve_provider_profile(config, &config.topology.provider)?;
        std::env::var(&profile.api_key_env).ok()?;
        Some(Self {
            profile: profile.clone(),
        })
    }
}

impl liberado_coder_agent::CoderProviderFactory for CoderRoleProviderFactory {
    fn provider_for(
        &self,
        _role: &str,
        config: &liberado_coder_core::CoderRoleConfig,
    ) -> Result<Arc<dyn Provider>, liberado_coder_core::CoderError> {
        let provider = OpenAiCompatibleProvider::from_env(
            &self.profile.api_key_env,
            self.profile.model_env.as_deref(),
            &self.profile.default_model,
            &self.profile.base_url,
            self.profile.extra_client_error_status.clone(),
        )
        .map_err(|e| liberado_coder_core::CoderError::Backend(e.to_string()))?;
        provider.set_model(config.model.clone());
        let provider = provider.with_reasoning_effort(config.reasoning.clone());
        if let Some(t) = config.temperature {
            return Ok(Arc::new(provider.with_temperature(Some(t))));
        }
        Ok(Arc::new(provider))
    }
}

/// The two subagent-tagged providers, built together — an `Option` pair where both are `Some` or
/// both `None`, so the `.expect("subagent_worker built alongside subagent")` unwraps that used to
/// exist at two sites are structurally impossible now.  The invariant was always true (the builder
/// constructs them from the same `ModelRole::Subagent` role), but it was a comment, not a type.
pub struct SubagentProviders {
    /// The `ModelRole::Subagent` backend the orchestrator uses as *its own* provider — journaled
    /// under `AgentRole::Orchestrator` (direct execution and the orchestrator's own tool loop).
    pub orchestrator: Arc<dyn Provider>,
    /// A second instance over the **same** backend, tagged `AgentRole::Subagent` so delegated
    /// subagent work journals under a separate role.
    pub worker: Arc<dyn Provider>,
}

/// The per-role providers the execution path uses, each already wrapped in a role-tagged
/// [`MeteredProvider`] so every inference call is recorded (latency observability). All five are
/// `Some` together (a provider is configured) or all `None` (watch-only).
///
/// `primary` is the unwrapped default provider — the one status/model display and the runtime
/// model-swap API act on. When a role declares no `[roles.<role>]` override it **shares the same
/// underlying provider** as `primary` (only the role tag differs), so an empty `[roles]` table is
/// exactly today's single-model behavior; a role only becomes a distinct backend/model/sampling when
/// it overrides something.
pub struct RoleProviders {
    pub primary: Option<Arc<dyn Provider>>,
    pub face: Option<Arc<dyn Provider>>,
    pub dispatcher: Option<Arc<dyn Provider>>,
    pub subagent: Option<SubagentProviders>,
}

impl RoleProviders {
    /// Watch-only when no provider is configured (no API key).
    pub fn is_enabled(&self) -> bool {
        self.primary.is_some()
    }

    /// Watch-only composition: no inference backends attached.
    pub fn none() -> Self {
        Self {
            primary: None,
            face: None,
            dispatcher: None,
            subagent: None,
        }
    }
}

/// Build the per-role providers from config, tagging each with its [`AgentRole`] and the shared
/// latency `recorder`. See [`RoleProviders`] for the sharing/override semantics.
pub fn role_providers_from_config(
    config: &Config,
    recorder: Arc<dyn LatencyRecorder>,
) -> RoleProviders {
    // The global default (base) provider. Absent key → watch-only.
    let Some(base_profile) = resolve_provider_profile(config, &config.topology.provider) else {
        return RoleProviders::none();
    };
    let Some(base) = build_provider_from_profile(base_profile, None) else {
        return RoleProviders::none();
    };
    tracing::info!(model = base.model(), provider = %base_profile.name, "provider configured (base)");

    let role_provider = |mrole: ModelRole, arole: AgentRole| -> Arc<dyn Provider> {
        let inner = match config.topology.roles.get(&mrole) {
            // Distinct backend/model/sampling only when this role overrides something.
            Some(ov)
                if ov.provider.is_some()
                    || ov.model.is_some()
                    || ov.temperature.is_some()
                    || ov.reasoning.is_some() =>
            {
                let profile = ov
                    .provider
                    .as_deref()
                    .and_then(|n| resolve_provider_profile(config, n))
                    .unwrap_or(base_profile);
                let built =
                    build_provider_from_profile(profile, Some(ov)).unwrap_or_else(|| base.clone());
                tracing::info!(
                    role = arole.as_str(),
                    model = built.model(),
                    provider = %profile.name,
                    temperature = ?ov.temperature,
                    reasoning = ?ov.reasoning.map(|r| r.as_str()),
                    "per-role provider override applied"
                );
                built
            }
            // Otherwise share the base provider (only the role tag differs).
            _ => base.clone(),
        };
        MeteredProvider::wrap(inner, arole, recorder.clone())
    };

    RoleProviders {
        face: Some(role_provider(ModelRole::MainAgent, AgentRole::Face)),
        dispatcher: Some(role_provider(ModelRole::Dispatcher, AgentRole::Dispatcher)),
        subagent: Some(SubagentProviders {
            orchestrator: role_provider(ModelRole::Subagent, AgentRole::Orchestrator),
            worker: role_provider(ModelRole::Subagent, AgentRole::Subagent),
        }),
        primary: Some(base),
    }
}

/// Build the `docker run` argv for a `McpTransport::Docker` MCP — `StdioConnector::new("docker",
/// argv)` then spawns it exactly like any other child process; MCP-over-stdio doesn't care whether
/// the process on the other end of the pipe is a bare binary or `docker run -i --rm ...`, so no
/// dedicated connector type is needed. `-i` (stdin attached) is required for MCP's stdio framing;
/// deliberately no `-t` (pseudo-TTY) — a TTY inserts `\r` into every line, corrupting the
/// newline-delimited JSON-RPC stream. `--rm` plus the child process dying (`ChildProcessTransport`'s
/// `kill_on_drop`, which breaks the attached stdin pipe and sends the container EOF) is enough to
/// clean up the container — no explicit `docker stop`/container-ID tracking needed.
pub(crate) fn docker_argv(
    image: &str,
    command: Option<&str>,
    args: &[String],
    volumes: &[String],
    env: &[String],
) -> Vec<String> {
    let mut argv = vec!["run".to_string(), "-i".to_string(), "--rm".to_string()];
    for volume in volumes {
        argv.push("--volume".to_string());
        argv.push(volume.clone());
    }
    for var in env {
        argv.push("--env".to_string());
        argv.push(var.clone());
    }
    argv.push(image.to_string());
    if let Some(command) = command {
        argv.push(command.to_string());
    }
    argv.extend(args.iter().cloned());
    argv
}

/// Build the MCP registry from the ENABLED MCPs in `config.topology.mcps` via the same
/// [`apply_mcp_peer_set`] transition used for hot-reload. `None` when no MCP is enabled
/// (decide-only daemon, tool-less chat) — callers that need a live handle for later reload should
/// use [`live_mcp_from_config`] instead (always returns a controller, possibly with an empty set).
///
/// When `health_catalog` is provided (the same `Arc` used for dispatcher routing), connect/transport
/// failures publish M1b **degraded** peer state so `routing_descriptors()` omits dead peers.
pub fn mcp_registry_from_config(
    config: &Config,
    health_catalog: Option<std::sync::Arc<CapabilityCatalog>>,
) -> Option<McpRegistry> {
    let controller = live_mcp_from_config(config, health_catalog);
    (!controller.registry().is_empty()).then_some(controller.registry())
}

fn pool_settings_from_config(config: &Config) -> McpPoolSettings {
    McpPoolSettings {
        enabled: config.tuning.mcp_pooling.enabled,
        idle_ttl: std::time::Duration::from_secs(config.tuning.mcp_pooling.idle_ttl_secs),
        max_in_flight_per_name: config.tuning.mcp_pooling.max_in_flight_per_name,
        connect_wait: std::time::Duration::from_secs(config.tuning.mcp_pooling.connect_wait_secs),
    }
}

/// Translate the declared `[topology.report_sink]` into the orchestrator's [`ReportSink`].
///
/// A plain shape conversion — every check that could reject it (MCP exists, is enabled, is not
/// read-only, and the tool really writes) already ran in `validate_merged_config`, so by the time
/// we are here the sink is known good. `None` simply means the deployment never declared one, and
/// vault delivery stays unavailable.
fn report_sink(topology: &liberado_config::Topology) -> Option<ReportSink> {
    let sink = topology.report_sink.as_ref()?;
    Some(ReportSink::new(
        &sink.mcp,
        &sink.tool,
        &sink.path_arg,
        &sink.content_arg,
    ))
}

/// Pre-resolve every enabled `[[session_profiles]]` entry into a `(profile_name → CapabilitySet)`
/// map the daemon uses at reaction time to narrow a session's authority from the pool ceiling to
/// the profile's component grant. The server path resolves profiles dynamically via
/// `state.config`; the daemon path resolves eagerly at boot because the daemon doesn't hold a
/// config reference.
fn session_profile_caps(config: &Config) -> HashMap<String, CapabilitySet> {
    config
        .topology
        .session_profiles
        .iter()
        .filter(|p| p.enabled)
        .map(|p| {
            let component = p.component_key();
            let caps = config.policy.capabilities_for(component);
            (p.name.clone(), caps)
        })
        .collect()
}

/// Construct the live MCP controller: shared [`CapabilityCatalog`] + cloneable [`McpRegistry`],
/// peers applied from `config.topology.mcps` (boot = first apply). Prefer this over
/// [`mcp_registry_from_config`] when hot-reload must remain possible even if the initial set is empty.
///
/// If `health_catalog` is `None`, a fresh catalog is created and seeded by the apply. Pass the
/// process-wide catalog so routing and health stay one object.
pub fn live_mcp_from_config(
    config: &Config,
    health_catalog: Option<Arc<CapabilityCatalog>>,
) -> LiveMcpController {
    let catalog = health_catalog.unwrap_or_else(|| Arc::new(CapabilityCatalog::new()));
    let registry = McpRegistry::with_pool_settings(pool_settings_from_config(config))
        .with_health_catalog(catalog.clone());
    let controller = LiveMcpController::new(catalog, registry);
    // Boot apply: empty → desired. Reject is unexpected after Config::validate; log and leave empty.
    if let Err(e) = controller.apply_config(config) {
        tracing::error!(error = %e, "initial MCP peer apply failed — starting with empty peer set");
    }
    controller
}

/// Build a [`liberado_cron::CronEventSource`] from the enabled entries in
/// `config.topology.schedules` (Decision 18/19). `None` when there are none enabled — a daemon then
/// behaves exactly as before this existed (vault-watch only). Construction only fails on a
/// malformed cron expression or duplicate name, which [`Config::validate`] should already have
/// caught (Decision 14 fail-fast) before this is ever called — surfaced anyway rather than assumed.
pub fn cron_source_from_config(
    config: &Config,
) -> Result<Option<liberado_cron::CronEventSource>, liberado_cron::CronError> {
    let schedules: Vec<liberado_cron::Schedule> = config
        .topology
        .schedules
        .iter()
        .filter(|s| s.enabled)
        .map(|s| liberado_cron::Schedule {
            name: s.name.clone(),
            cron_expr: s.cron_expr.clone(),
            goal: s.goal.clone(),
            pool: s.pool.clone(),
            profile: s.profile.clone(),
            deliver: s.deliver,
            max_turns: s.max_turns,
        })
        .collect();
    if schedules.is_empty() {
        return Ok(None);
    }
    Ok(Some(liberado_cron::CronEventSource::new(schedules)?))
}

/// Attach the dispatcher and (when an MCP server is configured) the orchestrator to `daemon`, using
/// the loaded `config`, the shared, live `catalog` (built once by the caller via
/// [`capability_catalog_from_config`] and also handed to chat's own dispatch and the server's API —
/// one object, not three independent snapshots), and `vault_path` (the same resolved path `daemon`
/// itself was opened over — not re-derived from `config.topology.vault_path`, since a CLI override
/// can make those differ). With no provider the daemon stays watch-only; with a provider but no MCP
/// it is decide-only. This is the single owner of the daemon's decide/act wiring.
///
/// `mcp` is the **shared** live [`McpRegistry`] (from [`LiveMcpController`]); clones are cheap and
/// hot-reload updates every pool. Pass an empty registry for decide-only.
///
/// The dispatcher is built from `config.tuning`, and the `"dispatcher"` component's capabilities
/// (`config.policy.capabilities_for("dispatcher")` — the union of grants naming that component) are
/// its maximal authority, so the Decision 4 boundary is now *configured* rather than empty. The
/// same set also bounds the orchestrator's `ExecuteDirect` runtime, so a decision the dispatcher
/// approved can't reach an MCP outside what `"dispatcher"` was actually granted. The orchestrator's
/// MCP connection comes from `topology.mcps` too (single source with the catalog), so routing and
/// execution line up by name.
///
/// `guidance` is the dispatcher's procedural-memory seam (`liberado-dispatch-logic-spec.md` §2
/// steps 1/5) — `None` means every `Dispatcher` built here behaves exactly as it did before this
/// parameter existed. The caller (`liberado-server`'s `run`) constructs it, if at all, since
/// building one means opening a vault-backed store and loading an embedding model — this crate
/// stays free of that dependency weight and decision.
/// Inbox folder always belongs in the watcher's whitelist. Extra `capture_paths` (pinned
/// widget file, additional folders, globs) are appended without duplicating `inbox_path`.
fn watcher_capture_paths(inbox_path: &str, extra: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    let inbox = inbox_path.trim();
    if !inbox.is_empty() {
        paths.push(inbox_path.to_string());
    }
    for p in extra {
        let trimmed = p.trim();
        if trimmed.is_empty() {
            continue;
        }
        let already = paths.iter().any(|existing| {
            existing == p || existing.trim_end_matches('/') == trimmed.trim_end_matches('/')
        });
        if !already {
            paths.push(p.clone());
        }
    }
    paths
}

pub fn configure_daemon(
    daemon: Daemon,
    providers: &RoleProviders,
    config: &Config,
    catalog: Arc<CapabilityCatalog>,
    mcp: McpRegistry,
    vault_path: &Path,
    guidance: Option<Arc<dyn ToolGuidanceSource>>,
) -> Daemon {
    // Timezone is config-only and useful even in watch-only mode (future notifiers / hooks).
    let daemon = apply_timezone(daemon, config);

    // Provider-independent knobs: proposal reaper and session-profile grants still matter in
    // watch-only mode (leftover proposals in the vault; fail-closed profile map if a source
    // injects events without a full dispatcher stack). Apply before the provider early-return so
    // `reap_interval_secs = 0` actually disables the reaper when no API key is set.
    let daemon = daemon
        .with_proposal_reap_interval(config.tuning.proposals.reap_interval_secs)
        .with_session_profile_caps(session_profile_caps(config))
        .with_inbox_ignore_globs(config.tuning.capture.inbox_ignore_globs.clone())
        .with_capture_scope(
            watcher_capture_paths(
                &config.tuning.capture.inbox_path,
                &config.tuning.capture.capture_paths,
            ),
            config.tuning.capture.ready_flag.clone(),
            config.tuning.capture.hold_flag.clone(),
        );

    let (Some(dispatcher_provider), Some(subagent_providers)) =
        (providers.dispatcher.as_ref(), providers.subagent.as_ref())
    else {
        tracing::warn!(
            "provider not configured (API key unset) — running watch-only (no dispatch)"
        );
        return daemon;
    };
    wire_dispatch_stack(
        daemon,
        dispatcher_provider,
        subagent_providers,
        config,
        catalog,
        mcp,
        vault_path,
        guidance.as_ref(),
    )
}

/// Wire every additional named pool (Decision 18 checkpoint #3) — same provider/tuning/consequence
/// catalog/proposals dir/signer as the default pool, differing only in the capability grant named
/// after the pool. MCP runtime is the **shared** live registry (clone), so peer hot-reload stays
/// one transition for every pool.
#[allow(clippy::too_many_arguments)]
fn attach_pools(
    daemon: Daemon,
    config: &Config,
    dispatcher_provider: &Arc<dyn liberado_provider::Provider>,
    guidance: Option<&Arc<dyn ToolGuidanceSource>>,
    catalog: &Arc<CapabilityCatalog>,
    orchestrator_infra: &OrchestratorInfra,
    notifier: Option<&Arc<dyn Notifier>>,
    mcp: &McpRegistry,
) -> Daemon {
    config
        .topology
        .pools
        .iter()
        .filter(|p| p.enabled)
        .fold(daemon, |daemon, pool| {
            wire_extra_pool(
                daemon,
                &pool.name,
                dispatcher_provider,
                config,
                guidance,
                catalog,
                orchestrator_infra,
                notifier,
                mcp,
            )
        })
}

/// Wire the full dispatch stack — dispatcher, orchestrator infra, notifier, cron, the default
/// pool's orchestrator, and every additional named pool — onto the daemon.
#[allow(clippy::too_many_arguments)]
fn wire_dispatch_stack(
    daemon: Daemon,
    dispatcher_provider: &Arc<dyn liberado_provider::Provider>,
    subagent_providers: &SubagentProviders,
    config: &Config,
    catalog: Arc<CapabilityCatalog>,
    mcp: McpRegistry,
    vault_path: &Path,
    guidance: Option<&Arc<dyn ToolGuidanceSource>>,
) -> Daemon {
    let dispatcher = build_dispatcher(dispatcher_provider, config, guidance);
    let capabilities = config.policy.capabilities_for("dispatcher");
    tracing::info!(
        grants = config.policy.grants.len(),
        capabilities = capabilities.capabilities.len(),
        "dispatcher capability boundary configured from policy"
    );
    // Runtime-level gating ingredients for the orchestrator's adaptive (non-seed) tool calls — the
    // same consequence catalog, vault-rooted proposals directory, and integrity signer chat's own
    // RiskGatedToolRuntime uses (see `RiskGatedToolRuntime`'s doc comment).
    let guard = guard_context(&catalog, &config.policy, vault_path);
    let orchestrator_infra = build_orchestrator_infra(subagent_providers, config, &catalog, &guard);
    // Optional — a daemon/orchestrator with no LIBERADO_TELEGRAM_* vars set just never
    // sends anything, same as before this existed. The motivating case is exactly this daemon
    // path: an unattended (cron-triggered, Phase 3) proposal nobody's watching the vault for.
    let notifier: Option<Arc<dyn Notifier>> =
        liberado_notify::TelegramNotifier::from_env().map(|n| Arc::new(n) as Arc<dyn Notifier>);
    tracing::info!(enabled = notifier.is_some(), "proposal notifications");

    // Reaper interval + profile caps already applied above (watch-only-safe path).
    let daemon = daemon
        .with_dispatcher(
            dispatcher,
            catalog.clone(),
            capabilities.clone(),
            guard.zone_write_classes.clone(),
        )
        .with_proposal_signer(guard.signer.clone())
        // Approval decisions live outside the vault, where no MCP mounts and no tool reaches. A
        // daemon without this executes nothing — see `Daemon::with_approval_ledger`.
        .with_approval_ledger(liberado_common::ApprovalLedger::new(
            liberado_config::data_dir(),
        ));
    let daemon = attach_notifier(daemon, &notifier);
    let daemon = attach_cron(daemon, config);
    // Always wire the shared live registry (even when empty) so empty→add hot-reload can acquire
    // peers without a process restart. Emptiness only affects acquisition, not composition.
    tracing::info!(
        peers = mcp.len(),
        "orchestrator enabled (live MCP registry; empty set is decide-until-reload)"
    );
    let orchestrator = with_orchestrator_notifier(
        orchestrator_infra.for_pool(mcp.clone(), capabilities, DEFAULT_POOL),
        &notifier,
    );
    let daemon = daemon.with_orchestrator(orchestrator);
    attach_pools(
        daemon,
        config,
        dispatcher_provider,
        guidance,
        &catalog,
        &orchestrator_infra,
        notifier.as_ref(),
        &mcp,
    )
}

/// Apply the operator timezone; an invalid name is logged (validate() already refused it at
/// load) and the daemon runs without Local-time stamps.
fn apply_timezone(daemon: Daemon, config: &Config) -> Daemon {
    match config.topology.user_timezone() {
        Ok(tz) => {
            tracing::info!(timezone = %tz.iana_name(), "operator timezone configured");
            daemon.with_user_timezone(tz)
        }
        Err(e) => {
            tracing::error!(error = %e, "topology.timezone invalid — cron/webhook goals will not get Local time stamps");
            daemon
        }
    }
}

/// A dispatcher over the router provider with the shared tunables and optional guidance.
fn build_dispatcher(
    dispatcher_provider: &Arc<dyn liberado_provider::Provider>,
    config: &Config,
    guidance: Option<&Arc<dyn ToolGuidanceSource>>,
) -> Dispatcher {
    let mut dispatcher = Dispatcher::new(
        dispatcher_provider.clone(),
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
    );
    if let Some(g) = guidance {
        dispatcher = dispatcher.with_guidance(g.clone());
    }
    dispatcher
}

/// Attach the telegram notifier when the environment configured one.
fn attach_notifier(daemon: Daemon, notifier: &Option<Arc<dyn Notifier>>) -> Daemon {
    match notifier {
        Some(n) => daemon.with_notifier(n.clone()),
        None => daemon,
    }
}

/// Attach the cron event source when schedules are configured; a construction failure is logged
/// and the daemon runs without cron.
fn attach_cron(daemon: Daemon, config: &Config) -> Daemon {
    match cron_source_from_config(config) {
        Ok(Some(cron_source)) => {
            tracing::info!(
                schedules = config.topology.schedules.len(),
                "cron event source attached"
            );
            daemon.with_cron_source(Box::new(cron_source))
        }
        Ok(None) => daemon,
        Err(e) => {
            tracing::error!(error = %e, "cron schedules failed to construct — running without cron");
            daemon
        }
    }
}

/// Build the orchestrator infrastructure shared by every pool (and the dispatch pack): the worker
/// provider, live consequence/zone catalog, proposals dir, and integrity signer.
fn build_orchestrator_infra(
    subagent_providers: &SubagentProviders,
    config: &Config,
    catalog: &Arc<CapabilityCatalog>,
    guard: &GuardContext,
) -> OrchestratorInfra {
    let mut infra = OrchestratorInfra::new(
        subagent_providers.orchestrator.clone(),
        catalog.clone(),
        guard.zone_write_classes.clone(),
        guard.proposals_dir.clone(),
        guard.signer.clone(),
    )
    .with_subagent_provider(subagent_providers.worker.clone());
    if let Some(max_turns) = config.topology.research_max_turns {
        infra = infra.with_research_max_turns(max_turns);
    }
    if let Some(sink) = report_sink(&config.topology) {
        infra = infra.with_report_sink(sink);
    }
    infra
}

/// Wire one additional named pool: its own dispatcher (same provider/tuning/guidance) and
/// orchestrator (same infra, its own capability grant and MCP registry clone).
#[allow(clippy::too_many_arguments)]
fn wire_extra_pool(
    daemon: Daemon,
    pool_name: &str,
    dispatcher_provider: &Arc<dyn liberado_provider::Provider>,
    config: &Config,
    guidance: Option<&Arc<dyn ToolGuidanceSource>>,
    catalog: &Arc<CapabilityCatalog>,
    orchestrator_infra: &OrchestratorInfra,
    notifier: Option<&Arc<dyn Notifier>>,
    mcp: &McpRegistry,
) -> Daemon {
    let pool_capabilities = config.policy.capabilities_for(pool_name);
    tracing::info!(
        pool = pool_name,
        capabilities = pool_capabilities.capabilities.len(),
        "additional pool capability boundary configured from policy"
    );
    let mut pool_dispatcher = Dispatcher::new(
        dispatcher_provider.clone(),
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
    );
    if let Some(g) = guidance {
        pool_dispatcher = pool_dispatcher.with_guidance(g.clone());
    }
    let daemon = daemon.with_pool_dispatcher(
        pool_name.to_string(),
        pool_dispatcher,
        catalog.clone(),
        pool_capabilities.clone(),
    );
    let orchestrator =
        orchestrator_infra.for_pool(mcp.clone(), pool_capabilities, pool_name.to_string());
    let orchestrator = match notifier {
        Some(n) => orchestrator.with_notifier(n.clone()),
        None => orchestrator,
    };
    daemon.with_pool_orchestrator(pool_name.to_string(), orchestrator)
}

/// Apply the optional notifier to an orchestrator, or return it unchanged.
fn with_orchestrator_notifier(
    orchestrator: Orchestrator,
    notifier: &Option<Arc<dyn Notifier>>,
) -> Orchestrator {
    match notifier {
        Some(n) => orchestrator.with_notifier(n.clone()),
        None => orchestrator,
    }
}

/// Build the [`DispatchPack`] that hosts dispatcher+orchestrator as a goal-session pack (E2/E3).
///
/// Parallel construction to [`configure_daemon`]: same pools, same capability ceilings, and the
/// **same** shared [`McpRegistry`] (clone) so hot-reload stays consistent. Register the returned pack
/// on the `liberado_session::GoalSessionHub` and hand that hub to the daemon via
/// [`Daemon::with_goal_hub`](liberado_daemon::Daemon::with_goal_hub).
///
/// Returns `None` when there is no provider (watch-only) — without inference there is nothing to
/// dispatch.
pub fn build_dispatch_pack(
    providers: &RoleProviders,
    config: &Config,
    catalog: Arc<CapabilityCatalog>,
    mcp: McpRegistry,
    vault_path: &Path,
    guidance: Option<Arc<dyn ToolGuidanceSource>>,
) -> Option<DispatchPack> {
    // The dispatch pack needs both the router (dispatcher) and the worker (subagent) providers.
    let dispatcher_provider = providers.dispatcher.as_ref()?;
    let sp = providers.subagent.as_ref()?;
    let guard = guard_context(&catalog, &config.policy, vault_path);
    let orchestrator_infra = build_orchestrator_infra(sp, config, &catalog, &guard);
    let notifier: Option<Arc<dyn Notifier>> =
        liberado_notify::TelegramNotifier::from_env().map(|n| Arc::new(n) as Arc<dyn Notifier>);

    let capabilities = config.policy.capabilities_for("dispatcher");
    let dispatcher = build_dispatcher(dispatcher_provider, config, guidance.as_ref());

    let mut pack = DispatchPack::new(
        catalog.clone(),
        guard.zone_write_classes.clone(),
        DEFAULT_REACTION_DEPTH_FOR_PACK,
        guard.proposals_dir.clone(),
    );
    if let Some(n) = &notifier {
        pack = pack.with_notifier(n.clone());
    }

    // Always wire the live registry (empty peers still allow later hot-reload).
    if mcp.is_empty() {
        tracing::info!(
            "dispatch pack: live MCP registry empty at boot — ExecuteDirect gains tools after reload"
        );
    }
    let orchestrator = orchestrator_infra.for_pool(mcp.clone(), capabilities, DEFAULT_POOL);
    let orchestrator = match &notifier {
        Some(n) => orchestrator.with_notifier(n.clone()),
        None => orchestrator,
    };
    pack = pack.with_pool(DEFAULT_POOL, dispatcher, orchestrator);

    // Additional named pools — same shared registry clone.
    for pool_cfg in config.topology.pools.iter().filter(|p| p.enabled) {
        let pool_capabilities = config.policy.capabilities_for(&pool_cfg.name);
        let pool_dispatcher = build_dispatcher(dispatcher_provider, config, guidance.as_ref());
        let orchestrator = with_orchestrator_notifier(
            orchestrator_infra.for_pool(mcp.clone(), pool_capabilities, pool_cfg.name.clone()),
            &notifier,
        );
        pack = pack.with_pool(pool_cfg.name.clone(), pool_dispatcher, orchestrator);
    }

    Some(pack)
}

/// Matches the daemon's default reaction depth (first agent step reacting to an external change).
const DEFAULT_REACTION_DEPTH_FOR_PACK: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::capability::Consequence;
    use liberado_config::{McpConfig, McpTransport};

    // These mirror the existing `*_from_env_uses_environment_variables`-style tests in
    // `liberado-provider-openai-compat`: they don't mutate process env vars (races under parallel
    // test execution), they just assert `provider_from_config` routes to the same underlying
    // `from_env()` call the config selects, whatever this process's env happens to be.
    #[test]
    fn unknown_provider_name_falls_back_to_deepseek_selection() {
        // Exercises `provider_from_config`'s own defensive fallback directly (bypassing
        // `Config::validate`, which would already reject this shape at load time) — the fallback
        // is a real, independent safety net, not just relying on validation having run first.
        let mut config = Config::default();
        config.topology.provider = "not-a-real-provider".to_string();
        assert_eq!(
            provider_from_config(&config).is_some(),
            OpenAiCompatibleProvider::deepseek_from_env().is_ok()
        );
    }

    #[test]
    fn openrouter_provider_name_routes_to_openrouter() {
        let mut config = Config::default();
        config.topology.provider = "openrouter".to_string();
        assert_eq!(
            provider_from_config(&config).is_some(),
            OpenAiCompatibleProvider::openrouter_from_env().is_ok()
        );
    }

    #[test]
    fn a_provider_declared_purely_via_config_is_selectable_with_no_dedicated_rust_type() {
        // The actual payoff of collapsing provider-deepseek/provider-openrouter into one generic
        // crate: a backend with no Rust wrapper at all becomes usable purely by adding a
        // `ProviderProfile` entry and pointing `topology.provider` at it.
        use liberado_config::ProviderProfile;

        let mut config = Config::default();
        config.topology.providers.push(ProviderProfile {
            name: "made-up-backend".to_string(),
            base_url: "https://example.invalid".to_string(),
            default_model: "some-model".to_string(),
            api_key_env: "LIBERADO_TEST_MADE_UP_BACKEND_KEY_DOES_NOT_EXIST".to_string(),
            model_env: None,
            extra_client_error_status: Vec::new(),
        });
        config.topology.provider = "made-up-backend".to_string();

        // No key set for this made-up backend, so construction fails cleanly (`None`) rather than
        // falling back to deepseek — proves the lookup actually found and used the new entry
        // instead of silently ignoring it.
        assert!(provider_from_config(&config).is_none());
    }

    fn mcp(name: &str, enabled: bool, transport: McpTransport) -> McpConfig {
        McpConfig {
            name: name.into(),
            enabled,
            description: "test".into(),
            consequence: Consequence::Reversible,
            transport,
            default_zone: None,
            tools: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
            writes_vault: Some(false),
        }
    }

    #[test]
    fn registry_registers_each_enabled_mcp_by_name() {
        let mut config = Config::default();
        config.topology.mcps = vec![
            mcp(
                "tasks-mcp",
                true,
                McpTransport::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@scope/tasks".into()],
                },
            ),
            mcp(
                "wiki-mcp",
                true,
                McpTransport::Http {
                    url: "https://mcp.deepwiki.com/mcp".into(),
                },
            ),
            // Disabled => must not be registered, so the dispatcher can't route to a dead name.
            mcp(
                "email-mcp",
                false,
                McpTransport::Stdio {
                    command: "email-mcp".into(),
                    args: vec![],
                },
            ),
        ];

        let registry = mcp_registry_from_config(&config, None).expect("two enabled MCPs => Some");
        let mut names = registry.names();
        names.sort_unstable();
        assert_eq!(names, vec!["tasks-mcp".to_string(), "wiki-mcp".to_string()]);
    }

    #[test]
    fn managed_transport_registers_by_name_too() {
        let mut config = Config::default();
        config.topology.mcps = vec![mcp("weather-mcp", true, McpTransport::Managed)];

        let registry = mcp_registry_from_config(&config, None).expect("one enabled MCP => Some");
        let names = registry.names();
        assert_eq!(names, vec!["weather-mcp".to_string()]);
    }

    #[test]
    fn docker_transport_registers_by_name_too() {
        let mut config = Config::default();
        config.topology.mcps = vec![mcp(
            "tasks-mcp-docker",
            true,
            McpTransport::Docker {
                image: "liberado-tasks-mcp:latest".into(),
                command: None,
                args: Vec::new(),
                volumes: Vec::new(),
                env: Vec::new(),
            },
        )];

        let registry = mcp_registry_from_config(&config, None).expect("one enabled MCP => Some");
        let names = registry.names();
        assert_eq!(names, vec!["tasks-mcp-docker".to_string()]);
    }

    #[test]
    fn docker_argv_with_only_image() {
        assert_eq!(
            docker_argv("liberado-tasks-mcp:latest", None, &[], &[], &[]),
            vec!["run", "-i", "--rm", "liberado-tasks-mcp:latest"]
        );
    }

    #[test]
    fn docker_argv_with_command_and_args() {
        let args = vec!["-y".to_string(), "@scope/tasks".to_string()];
        assert_eq!(
            docker_argv("node:22-slim", Some("npx"), &args, &[], &[]),
            vec![
                "run",
                "-i",
                "--rm",
                "node:22-slim",
                "npx",
                "-y",
                "@scope/tasks"
            ]
        );
    }

    #[test]
    fn docker_argv_with_volumes() {
        let volumes = vec!["/home/shiloh/vault:/vault:ro".to_string()];
        assert_eq!(
            docker_argv("image", None, &[], &volumes, &[]),
            vec![
                "run",
                "-i",
                "--rm",
                "--volume",
                "/home/shiloh/vault:/vault:ro",
                "image"
            ]
        );
    }

    #[test]
    fn docker_argv_with_env() {
        let env = vec!["API_KEY".to_string(), "MODE=prod".to_string()];
        assert_eq!(
            docker_argv("image", None, &[], &[], &env),
            vec![
                "run",
                "-i",
                "--rm",
                "--env",
                "API_KEY",
                "--env",
                "MODE=prod",
                "image"
            ]
        );
    }

    #[test]
    fn docker_argv_with_everything_combined() {
        let args = vec!["serve".to_string()];
        let volumes = vec!["/host:/container".to_string()];
        let env = vec!["API_KEY".to_string()];
        assert_eq!(
            docker_argv("my-mcp:latest", Some("my-mcp"), &args, &volumes, &env),
            vec![
                "run",
                "-i",
                "--rm",
                "--volume",
                "/host:/container",
                "--env",
                "API_KEY",
                "my-mcp:latest",
                "my-mcp",
                "serve"
            ]
        );
    }

    #[test]
    fn no_enabled_mcp_yields_none() {
        let mut config = Config::default();
        config.topology.mcps = vec![mcp(
            "email-mcp",
            false,
            McpTransport::Stdio {
                command: "email-mcp".into(),
                args: vec![],
            },
        )];
        assert!(mcp_registry_from_config(&config, None).is_none());
    }

    fn cron_schedule(name: &str, enabled: bool) -> liberado_config::CronSchedule {
        liberado_config::CronSchedule {
            name: name.into(),
            enabled,
            cron_expr: "0 0 9 * * * *".into(),
            goal: "summarize today's decisions".into(),
            pool: None,
            profile: None,
            deliver: None,
            max_turns: None,
        }
    }

    #[test]
    fn no_schedules_yields_none() {
        let config = Config::default();
        assert!(cron_source_from_config(&config).unwrap().is_none());
    }

    #[test]
    fn disabled_schedules_are_excluded() {
        let mut config = Config::default();
        config.topology.schedules = vec![cron_schedule("nightly", false)];
        assert!(cron_source_from_config(&config).unwrap().is_none());
    }

    #[test]
    fn an_enabled_schedule_builds_a_cron_source() {
        let mut config = Config::default();
        config.topology.schedules = vec![cron_schedule("nightly", true)];
        assert!(cron_source_from_config(&config).unwrap().is_some());
    }

    /// Watch-only (no API key / no providers): reaper tuning and session-profile caps still apply.
    #[tokio::test]
    async fn watch_only_configure_honors_zero_reap_interval() {
        use liberado_common::CapabilityCatalog;
        use liberado_daemon::Daemon;
        use liberado_mcp::McpRegistry;
        use std::sync::Arc;
        use std::time::Duration;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let daemon = Daemon::open("test", dir.path()).await.unwrap();
        // Default open() uses 600s — prove configure overwrites it even without a provider.
        assert_eq!(daemon.proposal_reap_interval(), Duration::from_secs(600));

        let mut config = Config::default();
        config.topology.vault_path = dir.path().to_path_buf();
        config.tuning.proposals.reap_interval_secs = 0;

        let configured = configure_daemon(
            daemon,
            &RoleProviders::none(),
            &config,
            Arc::new(CapabilityCatalog::new()),
            McpRegistry::new(),
            dir.path(),
            None,
        );
        assert_eq!(
            configured.proposal_reap_interval(),
            Duration::ZERO,
            "reap_interval_secs = 0 must disable the reaper in watch-only mode"
        );
    }

    #[tokio::test]
    async fn watch_only_configure_honors_custom_reap_interval() {
        use liberado_common::CapabilityCatalog;
        use liberado_daemon::Daemon;
        use liberado_mcp::McpRegistry;
        use std::sync::Arc;
        use std::time::Duration;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let daemon = Daemon::open("test", dir.path()).await.unwrap();
        let mut config = Config::default();
        config.topology.vault_path = dir.path().to_path_buf();
        config.tuning.proposals.reap_interval_secs = 42;

        let configured = configure_daemon(
            daemon,
            &RoleProviders::none(),
            &config,
            Arc::new(CapabilityCatalog::new()),
            McpRegistry::new(),
            dir.path(),
            None,
        );
        assert_eq!(configured.proposal_reap_interval(), Duration::from_secs(42));
    }

    #[test]
    fn watcher_capture_paths_unions_inbox_and_extras() {
        let paths = watcher_capture_paths(
            "inbox/",
            &["inbox/".into(), "Inbox/Capture.md".into(), "".into()],
        );
        assert_eq!(
            paths,
            vec!["inbox/".to_string(), "Inbox/Capture.md".to_string()]
        );
    }

    #[tokio::test]
    async fn configure_daemon_applies_positive_scope_from_tuning() {
        use liberado_common::CapabilityCatalog;
        use liberado_daemon::Daemon;
        use liberado_mcp::McpRegistry;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("inbox")).unwrap();
        std::fs::write(dir.path().join("inbox/in.md"), "captured").unwrap();
        std::fs::create_dir_all(dir.path().join("knowledge")).unwrap();
        std::fs::write(dir.path().join("knowledge/out.md"), "unflagged").unwrap();
        std::fs::write(dir.path().join("knowledge/now.md"), "go #ready-now").unwrap();
        std::fs::write(dir.path().join("inbox/held.md"), "park #hold-off").unwrap();
        std::fs::create_dir_all(dir.path().join("Inbox")).unwrap();
        std::fs::write(dir.path().join("Inbox/Capture.md"), "widget jot").unwrap();

        let daemon = Daemon::open("test", dir.path()).await.unwrap();
        let mut config = Config::default();
        config.topology.vault_path = dir.path().to_path_buf();
        config.tuning.capture.capture_paths = vec!["Inbox/Capture.md".into()];

        let configured = configure_daemon(
            daemon,
            &RoleProviders::none(),
            &config,
            Arc::new(CapabilityCatalog::new()),
            McpRegistry::new(),
            dir.path(),
            None,
        );

        assert!(
            configured
                .process_change(std::path::Path::new("inbox/in.md"))
                .await
                .unwrap()
                .is_some(),
            "inbox_path must be in scope after configure_daemon"
        );
        assert!(
            configured
                .process_change(std::path::Path::new("knowledge/out.md"))
                .await
                .unwrap()
                .is_none(),
            "unflagged note outside capture paths must be dropped"
        );
        assert!(
            configured
                .process_change(std::path::Path::new("knowledge/now.md"))
                .await
                .unwrap()
                .is_some(),
            "ready_flag must promote a note anywhere"
        );
        assert!(
            configured
                .process_change(std::path::Path::new("inbox/held.md"))
                .await
                .unwrap()
                .is_none(),
            "hold_flag must park even inside inbox/"
        );
        assert!(
            configured
                .process_change(std::path::Path::new("Inbox/Capture.md"))
                .await
                .unwrap()
                .is_some(),
            "extra capture_paths entry must reach the daemon"
        );
    }
}
