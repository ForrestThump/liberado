//! Top-level Config: compose topology/policy/tuning and validate.

use liberado_common::{CapabilitySet, DEFAULT_POOL, Error, Result};
use serde::{Deserialize, Serialize};

use super::builder::ConfigBuilder;
use super::policy::Policy;
use super::topology::{McpTransport, Topology, empty_table};
use super::tuning::Tuning;

/// The fully-resolved configuration the daemon runs on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub topology: Topology,
    pub policy: Policy,
    pub tuning: Tuning,
}

// ---------------------------------------------------------------------------
// Validation — the model-level slice of the Decision 14 fail-fast contract.
// ---------------------------------------------------------------------------

// `CoderTuning` (the typed `[tuning.coder]` model, its defaults, and its validation) moved to
// `liberado_coder_core::tuning` (2026-07-11 alignment audit) so the config stack no longer
// depends on the coding pack. `Tuning::coder` above carries the raw section; the pack parses it.

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

    /// Resolve what a goal session should run as (session-focus S6).
    ///
    /// `profile` names a `[[session_profiles]]` entry; when absent (or naming no enabled profile),
    /// the session falls back to `domain_fallback` with the grant keyed by the domain itself — the
    /// pool rule, so `[[grants]] component = "coding"` bounds an unprofiled coding session.
    ///
    /// Returns `(domain, capabilities, overrides, max_idle_secs)`. The capability set is **the**
    /// authority boundary for the session and can only ever be narrowed from here (Decision 4).
    /// `max_idle_secs` comes from the profile when set (E5); the caller may still override with a
    /// per-goal value.
    pub fn resolve_session_profile(
        &self,
        profile: Option<&str>,
        domain_fallback: &str,
    ) -> (String, CapabilitySet, toml::Value, Option<u64>) {
        let found = profile.and_then(|name| {
            self.topology
                .session_profiles
                .iter()
                .find(|p| p.enabled && p.name == name)
        });
        match found {
            Some(p) => (
                p.domain.clone(),
                self.policy.capabilities_for(p.component_key()),
                p.overrides.clone(),
                p.max_idle_secs,
            ),
            None => (
                domain_fallback.to_string(),
                self.policy.capabilities_for(domain_fallback),
                empty_table(),
                None,
            ),
        }
    }

    /// Validate invariants checkable from the resolved model alone. The daemon's loader layers
    /// additional cross-cutting checks on top (port/socket collisions, dangling zone/secret
    /// refs, triggerless hooks). Returns the first violation found.
    pub fn validate(&self) -> Result<()> {
        if self.topology.vault_path.as_os_str().is_empty() {
            return Err(Error::Config("topology.vault_path is required".into()));
        }
        if let Err(e) = self.topology.user_timezone() {
            return Err(Error::Config(format!("topology.timezone: {e}")));
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
        // `tuning.coder` is opaque here; the coding pack parses + validates it via
        // `liberado_coder_core::CoderTuning::from_value` at composition time (still fail-fast
        // at boot, just in the pack's parser — see the field's doc comment on `Tuning`).

        // Provider names must be unique, and `topology.provider` must actually name a declared
        // one — the same fail-fast shape as the model_roles check just below, so a typo'd or
        // removed provider name is a load-time error, not a runtime "provider silently unset."
        let mut seen_provider_names = std::collections::HashSet::new();
        for provider in &self.topology.providers {
            if !seen_provider_names.insert(&provider.name) {
                return Err(Error::Config(format!(
                    "topology.providers has a duplicate name '{}'",
                    provider.name
                )));
            }
        }
        if !self
            .topology
            .providers
            .iter()
            .any(|p| p.name == self.topology.provider)
        {
            return Err(Error::Config(format!(
                "topology.provider '{}' does not match any topology.providers entry",
                self.topology.provider
            )));
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

        // A Docker-transport MCP's image is the one thing that can't be a blank string — image
        // existence/daemon reachability are connect-time concerns (surfaced as an ordinary
        // RuntimeSetupError, same as a missing stdio binary), but an empty image is unambiguously
        // wrong under any interpretation, so it's rejected here at load time instead.
        for mcp in &self.topology.mcps {
            if let McpTransport::Docker { image, .. } = &mcp.transport
                && image.trim().is_empty()
            {
                return Err(Error::Config(format!(
                    "topology.mcps['{}'].transport.image must not be empty",
                    mcp.name
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
            if let Some(pool) = &schedule.pool
                && !pool_exists(pool)
            {
                return Err(Error::Config(format!(
                    "topology.schedules['{}'].pool '{pool}' does not name \"default\" or a \
                         declared, enabled topology.pools entry",
                    schedule.name
                )));
            }
        }
        for hook in &self.topology.hooks {
            if let Some(pool) = &hook.pool
                && !pool_exists(pool)
            {
                return Err(Error::Config(format!(
                    "topology.hooks['{}'].pool '{pool}' does not name \"default\" or a \
                         declared, enabled topology.pools entry",
                    hook.name
                )));
            }
        }

        // Session profiles (S6): unique names, a non-empty domain, and a capability grant that
        // actually exists. A profile whose component names no grant would silently run with ZERO
        // authority — the fail-safe default is correct but a silent one here is almost always a
        // typo, so it fails fast like every other config reference (Decision 14).
        let mut seen_profile_names = std::collections::HashSet::new();
        for profile in &self.topology.session_profiles {
            if !seen_profile_names.insert(profile.name.as_str()) {
                return Err(Error::Config(format!(
                    "topology.session_profiles has a duplicate name '{}'",
                    profile.name
                )));
            }
            if profile.domain.trim().is_empty() {
                return Err(Error::Config(format!(
                    "topology.session_profiles['{}'].domain is empty — it must name a domain pack \
                     (e.g. \"life\", \"coding\")",
                    profile.name
                )));
            }
            if !profile.enabled {
                continue;
            }
            let component = profile.component_key();
            if !self.policy.grants.iter().any(|g| g.component == component) {
                return Err(Error::Config(format!(
                    "topology.session_profiles['{}'].component '{component}' names no \
                     policy.toml [[grants]] entry — the session would run with zero authority",
                    profile.name
                )));
            }
        }

        Ok(())
    }
}
