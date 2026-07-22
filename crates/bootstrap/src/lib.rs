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

use std::path::Path;
use std::sync::Arc;

use liberado_common::{CapabilityCatalog, DEFAULT_POOL, ModelRole, ToolGuidanceSource};
use liberado_config::{McpTransport, ProviderProfile, RoleOverride, managed_binary_path};
use liberado_daemon::Daemon;
use liberado_dispatch_pack::DispatchPack;
use liberado_dispatcher::Dispatcher;
use liberado_mcp::{HttpConnector, McpRegistry, StdioConnector};
use liberado_notify::Notifier;
use liberado_orchestrator::OrchestratorInfra;
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

/// The per-role providers the execution path uses, each already wrapped in a role-tagged
/// [`MeteredProvider`] so every inference call is recorded (latency observability). All four are
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
    pub subagent: Option<Arc<dyn Provider>>,
}

impl RoleProviders {
    /// Watch-only when no provider is configured (no API key).
    pub fn is_enabled(&self) -> bool {
        self.primary.is_some()
    }

    fn none() -> Self {
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
        subagent: Some(role_provider(ModelRole::Subagent, AgentRole::Orchestrator)),
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
fn docker_argv(
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

/// Build the MCP registry from the ENABLED MCPs in `config.topology.mcps`, registering each by its
/// `name` with a connector from its transport. `None` when no MCP is enabled (decide-only daemon,
/// tool-less chat). This shares ONE source (topology.mcps) with the dispatcher catalog, so a name the
/// dispatcher routes to is a name the runtime can actually connect to.
pub fn mcp_registry_from_config(config: &Config) -> Option<McpRegistry> {
    let registry = config.topology.mcps.iter().filter(|m| m.enabled).fold(
        McpRegistry::new(),
        |registry, m| match &m.transport {
            McpTransport::Stdio { command, args } => {
                registry.register(&m.name, StdioConnector::new(command.clone(), args.clone()))
            }
            McpTransport::Http { url } => {
                registry.register(&m.name, HttpConnector::new(url.clone()))
            }
            McpTransport::Managed => {
                let bin = managed_binary_path(&mcp_install_dir(), &m.name);
                registry.register(&m.name, StdioConnector::new(bin.to_string_lossy(), vec![]))
            }
            McpTransport::Docker {
                image,
                command,
                args,
                volumes,
                env,
            } => {
                let argv = docker_argv(image, command.as_deref(), args, volumes, env);
                registry.register(&m.name, StdioConnector::new("docker", argv))
            }
        },
    );
    (!registry.is_empty()).then_some(registry)
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
pub fn configure_daemon(
    daemon: Daemon,
    providers: &RoleProviders,
    config: &Config,
    catalog: Arc<CapabilityCatalog>,
    vault_path: &Path,
    guidance: Option<Arc<dyn ToolGuidanceSource>>,
) -> Daemon {
    // Timezone is config-only and useful even in watch-only mode (future notifiers / hooks).
    let daemon = match config.topology.user_timezone() {
        Ok(tz) => {
            tracing::info!(timezone = %tz.iana_name(), "operator timezone configured");
            daemon.with_user_timezone(tz)
        }
        Err(e) => {
            // validate() already rejects bad names at load; this is a belt-and-suspenders log.
            tracing::error!(error = %e, "topology.timezone invalid — cron/webhook goals will not get Local time stamps");
            daemon
        }
    };

    let (Some(dispatcher_provider), Some(subagent_provider)) =
        (providers.dispatcher.as_ref(), providers.subagent.as_ref())
    else {
        tracing::warn!(
            "provider not configured (API key unset) — running watch-only (no dispatch)"
        );
        return daemon;
    };
    let mut dispatcher = Dispatcher::new(
        dispatcher_provider.clone(),
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
    );
    if let Some(g) = &guidance {
        dispatcher = dispatcher.with_guidance(g.clone());
    }
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
    // Everything an `Orchestrator` needs that's identical across every pool (see
    // `OrchestratorInfra`'s doc comment) — built once here, then combined per pool below with just
    // that pool's own factory/capabilities/name.
    let orchestrator_infra = OrchestratorInfra::new(
        subagent_provider.clone(),
        guard.consequences.clone(),
        guard.zone_catalog.clone(),
        guard.zone_write_classes.clone(),
        guard.proposals_dir.clone(),
        guard.signer.clone(),
    );

    // Optional — a daemon/orchestrator with no LIBERADO_TELEGRAM_* vars set just never
    // sends anything, same as before this existed. The motivating case is exactly this daemon
    // path: an unattended (cron-triggered, Phase 3) proposal nobody's watching the vault for.
    let notifier: Option<Arc<dyn Notifier>> =
        liberado_notify::TelegramNotifier::from_env().map(|n| Arc::new(n) as Arc<dyn Notifier>);
    tracing::info!(enabled = notifier.is_some(), "proposal notifications");

    let daemon = daemon
        .with_dispatcher(
            dispatcher,
            catalog.clone(),
            capabilities.clone(),
            guard.zone_write_classes.clone(),
        )
        .with_proposal_signer(guard.signer.clone());
    let daemon = match &notifier {
        Some(n) => daemon.with_notifier(n.clone()),
        None => daemon,
    };
    let daemon = match cron_source_from_config(config) {
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
    };
    let daemon = match mcp_registry_from_config(config) {
        Some(factory) => {
            tracing::info!("orchestrator enabled (MCP execution)");
            let orchestrator = orchestrator_infra.for_pool(factory, capabilities, DEFAULT_POOL);
            let orchestrator = match &notifier {
                Some(n) => orchestrator.with_notifier(n.clone()),
                None => orchestrator,
            };
            daemon.with_orchestrator(orchestrator)
        }
        None => {
            tracing::warn!("no enabled MCP in topology.mcps — decide-only (no MCP execution)");
            daemon
        }
    };

    // Additional named pools (Decision 18 checkpoint #3) — same provider/tuning/consequence
    // catalog/proposals dir/signer as the default pool, differing only in the capability grant
    // named after the pool and (necessarily) their own `McpRegistry` instance, since registries
    // aren't `Clone`/shareable across orchestrators.
    config
        .topology
        .pools
        .iter()
        .filter(|p| p.enabled)
        .fold(daemon, |daemon, pool| {
            let pool_capabilities = config.policy.capabilities_for(&pool.name);
            tracing::info!(
                pool = pool.name,
                capabilities = pool_capabilities.capabilities.len(),
                "additional pool capability boundary configured from policy"
            );
            let mut pool_dispatcher = Dispatcher::new(
                dispatcher_provider.clone(),
                config.tuning.dispatch.clone(),
                config.tuning.concurrency.max_reaction_depth,
            );
            if let Some(g) = &guidance {
                pool_dispatcher = pool_dispatcher.with_guidance(g.clone());
            }
            let daemon = daemon.with_pool_dispatcher(
                pool.name.clone(),
                pool_dispatcher,
                catalog.clone(),
                pool_capabilities.clone(),
            );
            // Intentionally called again here (once per enabled pool, so N+1 total calls across
            // this function for N additional pools) rather than reusing the default pool's registry
            // built above: each pool needs its own, independently owned `McpRegistry` (registries
            // aren't `Clone`/shareable across orchestrators — see the `fold`'s own doc comment
            // above), so there is no cheaper way to get N separate registries than building each
            // from `config.topology.mcps` N times. Cheap in absolute terms (this reads the same
            // small, already-in-memory `config`, not a file or network round-trip), so not worth
            // caching or restructuring around.
            match mcp_registry_from_config(config) {
                Some(factory) => {
                    let orchestrator =
                        orchestrator_infra.for_pool(factory, pool_capabilities, pool.name.clone());
                    let orchestrator = match &notifier {
                        Some(n) => orchestrator.with_notifier(n.clone()),
                        None => orchestrator,
                    };
                    daemon.with_pool_orchestrator(pool.name.clone(), orchestrator)
                }
                None => {
                    tracing::warn!(
                        pool = pool.name,
                        "no enabled MCP in topology.mcps — pool is decide-only (no MCP execution)"
                    );
                    daemon
                }
            }
        })
}

/// Build the [`DispatchPack`] that hosts dispatcher+orchestrator as a goal-session pack (E2/E3).
///
/// Parallel construction to [`configure_daemon`]: same pools, same capability ceilings, own
/// `McpRegistry` instances (registries are not shareable). Register the returned pack on the
/// [`liberado_session::GoalSessionHub`] and hand that hub to the daemon via
/// [`Daemon::with_goal_hub`](liberado_daemon::Daemon::with_goal_hub).
///
/// Returns `None` when there is no provider (watch-only) — without inference there is nothing to
/// dispatch.
pub fn build_dispatch_pack(
    providers: &RoleProviders,
    config: &Config,
    catalog: Arc<CapabilityCatalog>,
    vault_path: &Path,
    guidance: Option<Arc<dyn ToolGuidanceSource>>,
) -> Option<DispatchPack> {
    // The dispatch pack needs both the router (dispatcher) and the worker (subagent) providers.
    let dispatcher_provider = providers.dispatcher.as_ref()?;
    let subagent_provider = providers.subagent.as_ref()?;
    let guard = guard_context(&catalog, &config.policy, vault_path);
    let orchestrator_infra = OrchestratorInfra::new(
        subagent_provider.clone(),
        guard.consequences.clone(),
        guard.zone_catalog.clone(),
        guard.zone_write_classes.clone(),
        guard.proposals_dir.clone(),
        guard.signer.clone(),
    );
    let notifier: Option<Arc<dyn Notifier>> =
        liberado_notify::TelegramNotifier::from_env().map(|n| Arc::new(n) as Arc<dyn Notifier>);

    let capabilities = config.policy.capabilities_for("dispatcher");
    let mut dispatcher = Dispatcher::new(
        dispatcher_provider.clone(),
        config.tuning.dispatch.clone(),
        config.tuning.concurrency.max_reaction_depth,
    );
    if let Some(g) = &guidance {
        dispatcher = dispatcher.with_guidance(g.clone());
    }

    let mut pack = DispatchPack::new(
        catalog.clone(),
        guard.zone_write_classes.clone(),
        DEFAULT_REACTION_DEPTH_FOR_PACK,
        guard.proposals_dir.clone(),
    );
    if let Some(n) = &notifier {
        pack = pack.with_notifier(n.clone());
    }

    // Default pool.
    match mcp_registry_from_config(config) {
        Some(factory) => {
            let orchestrator = orchestrator_infra.for_pool(factory, capabilities, DEFAULT_POOL);
            let orchestrator = match &notifier {
                Some(n) => orchestrator.with_notifier(n.clone()),
                None => orchestrator,
            };
            pack = pack.with_pool(DEFAULT_POOL, dispatcher, orchestrator);
        }
        None => {
            // Decide-only: still register a dispatcher with a no-MCP orchestrator so Clarify/Propose
            // work; ExecuteDirect will use NoMcpRuntime.
            let orchestrator =
                orchestrator_infra.for_pool(EmptyRuntimeFactory, capabilities, DEFAULT_POOL);
            pack = pack.with_pool(DEFAULT_POOL, dispatcher, orchestrator);
            tracing::warn!(
                "dispatch pack: no enabled MCP — decide-only (ExecuteDirect has no tools)"
            );
        }
    }

    // Additional named pools.
    for pool_cfg in config.topology.pools.iter().filter(|p| p.enabled) {
        let pool_capabilities = config.policy.capabilities_for(&pool_cfg.name);
        let mut pool_dispatcher = Dispatcher::new(
            dispatcher_provider.clone(),
            config.tuning.dispatch.clone(),
            config.tuning.concurrency.max_reaction_depth,
        );
        if let Some(g) = &guidance {
            pool_dispatcher = pool_dispatcher.with_guidance(g.clone());
        }
        match mcp_registry_from_config(config) {
            Some(factory) => {
                let orchestrator =
                    orchestrator_infra.for_pool(factory, pool_capabilities, pool_cfg.name.clone());
                let orchestrator = match &notifier {
                    Some(n) => orchestrator.with_notifier(n.clone()),
                    None => orchestrator,
                };
                pack = pack.with_pool(pool_cfg.name.clone(), pool_dispatcher, orchestrator);
            }
            None => {
                let orchestrator = orchestrator_infra.for_pool(
                    EmptyRuntimeFactory,
                    pool_capabilities,
                    pool_cfg.name.clone(),
                );
                pack = pack.with_pool(pool_cfg.name.clone(), pool_dispatcher, orchestrator);
            }
        }
    }

    Some(pack)
}

/// Matches the daemon's default reaction depth (first agent step reacting to an external change).
const DEFAULT_REACTION_DEPTH_FOR_PACK: u32 = 1;

/// A `RuntimeFactory` that never connects — used when topology has no MCPs so the pack can still
/// classify (and fail ExecuteDirect honestly with no tools).
struct EmptyRuntimeFactory;

#[async_trait::async_trait]
impl liberado_executor::RuntimeFactory for EmptyRuntimeFactory {
    async fn runtime_for(
        &self,
        _allowed_mcps: &[String],
        _provenance: liberado_common::WriteProvenance,
    ) -> Result<Box<dyn liberado_executor::ToolRuntime>, liberado_executor::RuntimeSetupError> {
        Err(liberado_executor::RuntimeSetupError(
            "no MCP is configured in topology.mcps".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use liberado_common::capability::Consequence;
    use liberado_config::McpConfig;

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

        let registry = mcp_registry_from_config(&config).expect("two enabled MCPs => Some");
        let mut names: Vec<&str> = registry.names().collect();
        names.sort_unstable();
        assert_eq!(names, vec!["tasks-mcp", "wiki-mcp"]);
    }

    #[test]
    fn managed_transport_registers_by_name_too() {
        let mut config = Config::default();
        config.topology.mcps = vec![mcp("weather-mcp", true, McpTransport::Managed)];

        let registry = mcp_registry_from_config(&config).expect("one enabled MCP => Some");
        let names: Vec<&str> = registry.names().collect();
        assert_eq!(names, vec!["weather-mcp"]);
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

        let registry = mcp_registry_from_config(&config).expect("one enabled MCP => Some");
        let names: Vec<&str> = registry.names().collect();
        assert_eq!(names, vec!["tasks-mcp-docker"]);
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
        assert!(mcp_registry_from_config(&config).is_none());
    }

    fn cron_schedule(name: &str, enabled: bool) -> liberado_config::CronSchedule {
        liberado_config::CronSchedule {
            name: name.into(),
            enabled,
            cron_expr: "0 0 9 * * * *".into(),
            goal: "summarize today's decisions".into(),
            pool: None,
            profile: None,
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
}
