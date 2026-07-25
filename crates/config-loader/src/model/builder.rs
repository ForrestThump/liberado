//! ConfigBuilder for tests and programmatic assembly.

use std::path::PathBuf;

use liberado_common::{ModelProfile, ModelRole, Result};

use super::config::Config;
use super::policy::{Grant, ZonePolicy};
use super::topology::{CronSchedule, HookConfig, McpConfig};
use super::tuning::Tuning;

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
    use liberado_common::{
        Capability, Consequence, DEFAULT_POOL, Error, ModelProfile, ModelRole, ModelTier,
        WriteClass, Zone,
    };
    use std::str::FromStr;

    use crate::model::policy::Policy;
    use crate::model::topology::{
        McpTransport, PoolConfig, ProviderProfile, SessionProfile, Topology, empty_table,
    };

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
        assert!(t.coder.is_none(), "no [tuning.coder] section by default");
        assert!(t.mcp_pooling.enabled, "MCP pooling defaults on");
        assert_eq!(t.mcp_pooling.idle_ttl_secs, 300);
        assert_eq!(t.mcp_pooling.max_in_flight_per_name, 4);
        assert_eq!(t.mcp_pooling.connect_wait_secs, 60);
    }

    // Typed `[tuning.coder]` parsing/validation tests moved to `liberado_coder_core::tuning`
    // with the `CoderTuning` type itself. What config-loader owns now is only raw passthrough:
    #[test]
    fn coder_section_is_carried_as_an_opaque_value() {
        let toml = r#"
[topology]
vault_path = "/vault"

[tuning.coder]
backend = "liberado-loop"
trace_dir = "traces"

[tuning.coder.coder]
model = "deepseek/deepseek-v4-pro"
prompt_path = "prompts/custom-coder.md"
max_turns = 44
"#;
        let cfg = Config::from_str(toml).expect("opaque coder section must not fail load");

        let coder = cfg.tuning.coder.as_ref().expect("section present");
        assert_eq!(
            coder.get("trace_dir").and_then(|v| v.as_str()),
            Some("traces")
        );
        assert_eq!(
            coder
                .get("coder")
                .and_then(|c| c.get("max_turns"))
                .and_then(|v| v.as_integer()),
            Some(44)
        );
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

    fn provider_profile(name: &str) -> ProviderProfile {
        ProviderProfile {
            name: name.into(),
            base_url: format!("https://{name}.example.com"),
            default_model: "some-model".into(),
            api_key_env: format!("{}_API_KEY", name.to_uppercase()),
            model_env: None,
            extra_client_error_status: Vec::new(),
        }
    }

    #[test]
    fn duplicate_provider_names_fail_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.providers = vec![provider_profile("deepseek"), provider_profile("deepseek")];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn topology_provider_must_match_a_declared_providers_entry() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.provider = "not-declared-anywhere".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_brand_new_provider_declared_purely_via_config_validates_and_is_selectable() {
        // The actual goal of this feature: a backend this system has never shipped with (no Rust
        // wrapper, no dedicated crate) becomes usable by adding one `ProviderProfile` entry and
        // pointing `topology.provider` at it — no code change.
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.providers.push(provider_profile("groq"));
        cfg.topology.provider = "groq".into();
        assert!(cfg.validate().is_ok());
        assert!(
            cfg.topology
                .providers
                .iter()
                .any(|p| p.name == "groq" && p.base_url == "https://groq.example.com")
        );
    }

    #[test]
    fn default_providers_seed_deepseek_and_openrouter() {
        let cfg = Config::default();
        let names: Vec<&str> = cfg
            .topology
            .providers
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(names, vec!["deepseek", "openrouter"]);
        assert_eq!(cfg.topology.provider, "deepseek");
    }

    #[test]
    fn default_timezone_is_america_chicago_and_resolves() {
        let cfg = Config::default();
        assert_eq!(cfg.topology.timezone, "America/Chicago");
        assert_eq!(
            cfg.topology.user_timezone().unwrap().iana_name(),
            "America/Chicago"
        );
    }

    #[test]
    fn invalid_timezone_fails_validate() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.timezone = "Not/ARealZone".into();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("topology.timezone"),
            "expected timezone validation error, got {err}"
        );
    }

    fn cron_schedule(name: &str, cron_expr: &str) -> CronSchedule {
        CronSchedule {
            name: name.into(),
            enabled: true,
            cron_expr: cron_expr.into(),
            goal: "do something".into(),
            pool: None,
            profile: None,
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

    // ── Session profiles (session-focus S6) ──────────────────────────────────

    fn profile(name: &str, domain: &str, component: Option<&str>) -> SessionProfile {
        SessionProfile {
            name: name.into(),
            enabled: true,
            domain: domain.into(),
            component: component.map(Into::into),
            max_idle_secs: None,
            overrides: empty_table(),
        }
    }

    /// A config with a narrow `research` hat and a normal `life` grant — the S6 worked example.
    fn config_with_profiles() -> Config {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.policy.grants = vec![
            Grant {
                component: "life".into(),
                capabilities: vec![
                    Capability::Write(Zone::vault("tasks")),
                    Capability::AskHuman,
                ],
            },
            Grant {
                component: "research".into(),
                capabilities: vec![Capability::Read(Zone::vault("tasks"))],
            },
        ];
        cfg.topology.session_profiles = vec![profile("research", "life", Some("research"))];
        cfg
    }

    #[test]
    fn a_profile_resolves_to_its_pack_and_its_own_narrower_grant() {
        let cfg = config_with_profiles();
        let (domain, caps, _overrides, _idle) =
            cfg.resolve_session_profile(Some("research"), "coding");

        // The profile picks the pack — the caller's fallback domain is overridden.
        assert_eq!(domain, "life");
        // ...and it holds strictly less than the default `life` grant: read-only, and crucially it
        // cannot interrupt a human.
        assert!(caps.contains(&Capability::Read(Zone::vault("tasks"))));
        assert!(!caps.contains(&Capability::Write(Zone::vault("tasks"))));
        assert!(
            !caps.grants_ask_human(),
            "the research hat must not be able to interrupt a human"
        );
    }

    #[test]
    fn no_profile_falls_back_to_the_grant_keyed_by_the_domain() {
        let cfg = config_with_profiles();
        let (domain, caps, _, _) = cfg.resolve_session_profile(None, "life");
        assert_eq!(domain, "life");
        assert!(caps.grants_ask_human(), "an attended life session may ask");
        assert!(caps.contains(&Capability::Write(Zone::vault("tasks"))));
    }

    #[test]
    fn an_unknown_profile_name_falls_back_rather_than_inventing_authority() {
        let cfg = config_with_profiles();
        let (domain, caps, _, _) = cfg.resolve_session_profile(Some("nonexistent"), "life");
        // Falls back to the domain's own grant — it must never synthesize capabilities.
        assert_eq!(domain, "life");
        assert_eq!(caps, cfg.policy.capabilities_for("life"));
    }

    #[test]
    fn a_domain_with_no_grant_at_all_resolves_to_zero_authority() {
        let cfg = config_with_profiles();
        let (_, caps, _, _) = cfg.resolve_session_profile(None, "coding");
        assert!(
            caps.capabilities.is_empty(),
            "fail safe: no grant, no authority"
        );
        assert!(!caps.grants_ask_human());
    }

    #[test]
    fn a_profile_whose_component_names_no_grant_fails_validation() {
        // Silently running with zero authority is the *safe* outcome but almost always a typo, so
        // it fails fast rather than leaving the user wondering why their hat can do nothing.
        let mut cfg = config_with_profiles();
        cfg.topology.session_profiles = vec![profile("research", "life", Some("typoed"))];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn duplicate_profile_names_fail_validation() {
        let mut cfg = config_with_profiles();
        cfg.topology
            .session_profiles
            .push(profile("research", "life", Some("research")));
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn a_profile_component_defaults_to_its_name() {
        let p = profile("research", "life", None);
        assert_eq!(p.component_key(), "research");
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
    fn a_schedule_naming_enabled_session_profile_passes_validation() {
        let mut cfg = config_with_profiles();
        let mut schedule = cron_schedule("morning", "0 0 9 * * * *");
        schedule.profile = Some("research".into());
        cfg.topology.schedules = vec![schedule];
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn a_schedule_naming_unknown_session_profile_fails_validation() {
        let mut cfg = config_with_profiles();
        let mut schedule = cron_schedule("morning", "0 0 9 * * * *");
        schedule.profile = Some("typo-not-a-profile".into());
        cfg.topology.schedules = vec![schedule];
        let err = cfg.validate().expect_err("unknown profile must fail");
        assert!(
            err.to_string().contains("profile"),
            "error should mention profile: {err}"
        );
    }

    #[test]
    fn a_schedule_naming_disabled_session_profile_fails_validation() {
        let mut cfg = config_with_profiles();
        cfg.topology.session_profiles[0].enabled = false;
        let mut schedule = cron_schedule("morning", "0 0 9 * * * *");
        schedule.profile = Some("research".into());
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
    fn docker_transport_with_only_image_deserializes_with_defaults() {
        let toml = r#"
[[mcps]]
name = "tasks-mcp-docker"
description = "tasks-mcp in a container"
consequence = "reversible"
transport = { kind = "docker", image = "liberado-tasks-mcp:latest" }
"#;
        let topology: Topology = toml::from_str(toml).expect("docker transport must deserialize");
        match &topology.mcps[0].transport {
            McpTransport::Docker {
                image,
                command,
                args,
                volumes,
                env,
            } => {
                assert_eq!(image, "liberado-tasks-mcp:latest");
                assert_eq!(command, &None);
                assert!(args.is_empty());
                assert!(volumes.is_empty());
                assert!(env.is_empty());
            }
            other => panic!("expected docker, got {other:?}"),
        }
    }

    #[test]
    fn docker_transport_with_all_fields_deserializes() {
        let toml = r#"
[[mcps]]
name = "tasks-mcp-docker"
description = "tasks-mcp in a container"
consequence = "reversible"
transport = { kind = "docker", image = "liberado-tasks-mcp:latest", command = "npx", args = ["-y", "@scope/tasks"], volumes = ["/home/shiloh/vault:/vault:ro"], env = ["API_KEY"] }
"#;
        let topology: Topology = toml::from_str(toml).expect("docker transport must deserialize");
        match &topology.mcps[0].transport {
            McpTransport::Docker {
                image,
                command,
                args,
                volumes,
                env,
            } => {
                assert_eq!(image, "liberado-tasks-mcp:latest");
                assert_eq!(command.as_deref(), Some("npx"));
                assert_eq!(args, &["-y", "@scope/tasks"]);
                assert_eq!(volumes, &["/home/shiloh/vault:/vault:ro"]);
                assert_eq!(env, &["API_KEY"]);
            }
            other => panic!("expected docker, got {other:?}"),
        }
    }

    #[test]
    fn docker_transport_with_blank_image_fails_validation() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/home/shiloh/vault");
        cfg.topology.mcps = vec![McpConfig {
            name: "broken-docker-mcp".into(),
            enabled: true,
            description: "should fail validation".into(),
            consequence: Consequence::Reversible,
            transport: McpTransport::Docker {
                image: "   ".to_string(),
                command: None,
                args: Vec::new(),
                volumes: Vec::new(),
                env: Vec::new(),
            },
            default_zone: None,
            tools: Vec::new(),
            zone_from_arg: None,
            write_tools: Vec::new(),
            writes_vault: Some(false),
        }];
        let err = cfg
            .validate()
            .expect_err("blank image must fail validation");
        assert!(
            err.to_string().contains("broken-docker-mcp"),
            "error should name the offending MCP: {err}"
        );
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
provider = "deepseek"

[[topology.mcps]]
name = "my-mcp"
description = "a test MCP"
consequence = "read_only"
transport = { kind = "stdio", command = "echo", args = ["hello"] }
"#;
        let cfg = Config::from_str(toml).expect("valid TOML should parse");
        assert_eq!(cfg.topology.vault_path, PathBuf::from("/home/test/vault"));
        assert_eq!(cfg.topology.provider, "deepseek");
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
            .provider("deepseek")
            .build()
            .expect("valid config");
        assert_eq!(cfg.topology.daemon_socket, PathBuf::from("/tmp/test.sock"));
        assert_eq!(cfg.topology.provider, "deepseek");
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
                zone_from_arg: None,
                write_tools: Vec::new(),
                writes_vault: Some(false),
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
