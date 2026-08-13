//! Top-level Config: compose topology/policy/tuning and validate.

use std::path::{Component, Path, PathBuf};

use liberado_common::{CapabilitySet, DEFAULT_POOL, Error, Result, WriteClass};
use serde::{Deserialize, Serialize};

use super::builder::ConfigBuilder;
use super::policy::Policy;
use super::topology::{
    McpGrant, McpTransport, ProjectConfig, SessionProfile, Topology, empty_table,
};

/// What a session profile resolves to — everything a caller needs to start a session under it.
///
/// A struct rather than the tuple this used to return: the tuple was already four wide, and profiles
/// now carry behaviour (model, prompt, delegation) as well as authority. A named struct also lets a
/// caller take only what it needs — `POST /api/goals` wants the domain, a chat wants the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedProfile {
    /// The profile's name, or `None` when no profile was named (the domain-fallback path).
    pub name: Option<String>,
    pub description: Option<String>,
    /// Which pack runs this session. `None` for a chat-only profile — a conversation has the face
    /// agent, not a pack, so a caller that *needs* a pack must reject it rather than invent one.
    pub domain: Option<String>,
    /// **The** authority boundary for the session; narrowable from here, never widenable.
    pub capabilities: CapabilitySet,
    /// Opaque, pack-parsed. Never interpreted by the config stack.
    pub overrides: toml::Value,
    pub max_idle_secs: Option<u64>,
    /// `None` = the daemon's default delegation mode.
    pub delegation: Option<bool>,
    /// `None` = the daemon's current model for the role.
    pub model: Option<String>,
    pub prompt_append: Option<String>,
}

impl ResolvedProfile {
    /// The kernel-side `SessionGrant` this profile runs as.
    ///
    /// Returns the parts rather than the type itself: `liberado-config-loader` sits below
    /// `liberado-session` and must not depend on it, so the caller assembles the struct. Keeping the
    /// *mapping* here is what stops three call sites from each deciding which fields to carry —
    /// forgetting one would silently drop a profile's model or its delegation setting.
    ///
    /// `overrides` is converted to JSON by the caller (it is `toml::Value` here, `serde_json::Value`
    /// there), which is the one piece this cannot hand over cleanly.
    pub fn grant_parts(&self) -> GrantParts<'_> {
        GrantParts {
            capabilities: self.capabilities.clone(),
            profile: self.name.clone(),
            delegation: self.delegation,
            model: self.model.as_deref(),
            prompt_append: self.prompt_append.as_deref(),
        }
    }
}

/// The pieces of a `SessionGrant` a resolved profile supplies. See [`ResolvedProfile::grant_parts`].
pub struct GrantParts<'a> {
    pub capabilities: CapabilitySet,
    pub profile: Option<String>,
    pub delegation: Option<bool>,
    pub model: Option<&'a str>,
    pub prompt_append: Option<&'a str>,
}

/// Hand-written rather than derived: `toml::Value` has no `Default`, and "no overrides" must
/// deserialize to an empty *table* rather than a null so packs parse it uniformly.
impl Default for ResolvedProfile {
    fn default() -> Self {
        Self {
            name: None,
            description: None,
            domain: None,
            capabilities: CapabilitySet::empty(),
            overrides: empty_table(),
            max_idle_secs: None,
            delegation: None,
            model: None,
            prompt_append: None,
        }
    }
}
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
    /// `profile` names a `[[session_profiles]]` entry. When it is `None`, the session falls back to
    /// `domain_fallback` with the grant keyed by the domain itself — the pool rule, so
    /// `[[grants]] component = "coding"` bounds an unprofiled coding session.
    ///
    /// **Fail-closed on a named profile that does not resolve.** A `Some(name)` matching no
    /// *enabled* entry is an error, never a fallback: the caller asked for a specific hat, and
    /// quietly handing back the domain's grant instead would widen a session past what the profile
    /// was written to allow — the exact hazard on a typo'd or newly-disabled profile. This mirrors
    /// the daemon's reactor (`liberado_daemon`'s `react`), which refuses to start a session on an
    /// unknown profile, and [`validate`](Self::validate), which rejects the same mistake in
    /// `topology.schedules` at load time. Runtime-created goals (HTTP `POST /api/goals`, Telegram
    /// `/spawn`) can only be caught here.
    ///
    /// Returns everything a caller needs to run under this profile. The capability set is **the**
    /// authority boundary for the session and can only ever be narrowed from here (Decision 4).
    /// `max_idle_secs` comes from the profile when set (E5); the caller may still override with a
    /// per-goal value.
    pub fn resolve_session_profile(
        &self,
        profile: Option<&str>,
        domain_fallback: &str,
    ) -> Result<ResolvedProfile> {
        let Some(name) = profile else {
            return Ok(ResolvedProfile {
                domain: Some(domain_fallback.to_string()),
                capabilities: self.policy.capabilities_for(domain_fallback),
                overrides: empty_table(),
                ..ResolvedProfile::default()
            });
        };
        let found = self
            .topology
            .session_profiles
            .iter()
            .find(|p| p.enabled && p.name == name);
        match found {
            Some(p) => Ok(ResolvedProfile {
                name: Some(p.name.clone()),
                description: p.description.clone(),
                domain: p.domain.clone(),
                capabilities: self.profile_capabilities(p),
                overrides: p.overrides.clone(),
                max_idle_secs: p.max_idle_secs,
                delegation: p.delegation,
                model: p.model.clone(),
                prompt_append: p.prompt_append.clone(),
            }),
            None => Err(Error::Config(format!(
                "session profile '{name}' does not name an enabled \
                 topology.session_profiles entry"
            ))),
        }
    }

    /// A profile's effective authority, in its two shapes.
    ///
    /// * **Declares nothing** → `capabilities_for(component_key())`. Unchanged from before profiles
    ///   could declare authority; the pool rule still applies.
    /// * **Declares something** → exactly that, narrowed against [`ceiling`](SessionProfile::ceiling)
    ///   when one is named. Narrowing is what keeps `policy.toml` a hard bound: a profile asking for
    ///   an MCP the ceiling lacks resolves to nothing rather than granting it.
    ///
    /// The narrowing is `CapabilitySet::narrow`, which understands that `ExecuteMcp` subsumes
    /// `ExecuteTool` — so a ceiling granting the whole `turbovault` server permits a profile that
    /// asks for two of its tools, which is the case this feature exists for.
    fn profile_capabilities(&self, profile: &SessionProfile) -> CapabilitySet {
        if !profile.declares_authority() {
            return self.policy.capabilities_for(profile.component_key());
        }
        let declared = profile.declared_capabilities();
        match &profile.ceiling {
            Some(key) => declared.narrow(&self.policy.capabilities_for(key)),
            None => declared,
        }
    }

    /// Every enabled profile, for a picker. Ordered as configured, so the operator controls the list.
    pub fn enabled_session_profiles(&self) -> Vec<&SessionProfile> {
        self.topology
            .session_profiles
            .iter()
            .filter(|p| p.enabled)
            .collect()
    }

    /// Every enabled coding project, for `GET /api/projects` and `/goal in` pickers.
    pub fn enabled_projects(&self) -> Vec<&ProjectConfig> {
        self.topology
            .projects
            .iter()
            .filter(|p| p.enabled)
            .collect()
    }

    /// Look up an enabled project by exact name (`/goal in <name>`).
    pub fn project_by_name(&self, name: &str) -> Option<&ProjectConfig> {
        self.topology
            .projects
            .iter()
            .find(|p| p.enabled && p.name == name)
    }

    /// Authorize a coding goal's `project` / `workspace_root` payload fields (coding-tui S3 / G4).
    ///
    /// * Neither field → [`CodingWorkspaceAuth::Ephemeral`] (temp workspace; always allowed).
    /// * `project` name → must match an enabled `[[projects]]` entry with a write class that allows
    ///   direct agent writes; optional `workspace_root` must sit under that project's root.
    /// * `workspace_root` alone → must canonicalize under some enabled project's root.
    ///
    /// Fail-closed: unknown names and undeclared paths are errors, never silently allowed.
    pub fn authorize_coding_workspace(
        &self,
        project: Option<&str>,
        workspace_root: Option<&str>,
    ) -> std::result::Result<CodingWorkspaceAuth, CodingAuthError> {
        let project = project.map(str::trim).filter(|s| !s.is_empty());
        let workspace_root = workspace_root.map(str::trim).filter(|s| !s.is_empty());

        match (project, workspace_root) {
            (None, None) => Ok(CodingWorkspaceAuth::Ephemeral),
            (Some(name), path_opt) => {
                let proj =
                    self.project_by_name(name)
                        .ok_or_else(|| CodingAuthError::UnknownProject {
                            name: name.to_string(),
                        })?;
                if !proj.write_class.allows_direct_agent_write() {
                    return Err(CodingAuthError::NotWritable {
                        name: proj.name.clone(),
                        write_class: proj.write_class,
                    });
                }
                let root = canonicalize_existing_dir(&proj.root).map_err(|reason| {
                    CodingAuthError::InvalidPath {
                        reason: format!(
                            "project '{}' root '{}': {reason}",
                            proj.name,
                            proj.root.display()
                        ),
                    }
                })?;
                let workspace = if let Some(path) = path_opt {
                    let cand = resolve_workspace_path(Path::new(path))
                        .map_err(|reason| CodingAuthError::InvalidPath { reason })?;
                    if !path_is_within(&root, &cand) {
                        return Err(CodingAuthError::UndeclaredWorkspace {
                            path: cand.display().to_string(),
                        });
                    }
                    cand
                } else {
                    root
                };
                Ok(CodingWorkspaceAuth::Project {
                    name: proj.name.clone(),
                    root: workspace,
                })
            }
            (None, Some(path)) => {
                let cand = resolve_workspace_path(Path::new(path))
                    .map_err(|reason| CodingAuthError::InvalidPath { reason })?;
                // Most-specific project wins — a narrower `agent_writable` entry under a broad
                // `proposal_only` umbrella must not be shadowed by the umbrella.
                let mut best: Option<(&ProjectConfig, usize)> = None;
                for proj in self.enabled_projects() {
                    let Ok(root) = canonicalize_existing_dir(&proj.root) else {
                        tracing::warn!(
                            project = %proj.name,
                            root = %proj.root.display(),
                            "skipping project with unreadable root during workspace auth"
                        );
                        continue;
                    };
                    if !path_is_within(&root, &cand) {
                        continue;
                    }
                    let depth = root.components().count();
                    if best.is_none_or(|(_, d)| depth > d) {
                        best = Some((proj, depth));
                    }
                }
                let Some((proj, _)) = best else {
                    return Err(CodingAuthError::UndeclaredWorkspace {
                        path: cand.display().to_string(),
                    });
                };
                if !proj.write_class.allows_direct_agent_write() {
                    return Err(CodingAuthError::NotWritable {
                        name: proj.name.clone(),
                        write_class: proj.write_class,
                    });
                }
                Ok(CodingWorkspaceAuth::Project {
                    name: proj.name.clone(),
                    root: cand,
                })
            }
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
        // `[tuning.capture]` parses and validates. `inbox_ignore_globs`, `inbox_path`,
        // `capture_paths`, `ready_flag`, and `hold_flag` are live — the F12 positive scope.
        // The remaining fields (settle windows, `processed_path`, `ambient_sweep_schedule`)
        // have no code reading them yet — see `CaptureTuning` doc. Warn rather than reject:
        // rejecting would break configs that already carry the section (including
        // `config.example/tuning.toml`), and the section is not wrong, just partially inert.
        let c = &self.tuning.capture;
        let default = super::tuning::CaptureTuning::default();
        let unimplemented_fields_differ = c.inbox_settle_window_secs
            != default.inbox_settle_window_secs
            || c.ready_now_settle_secs != default.ready_now_settle_secs
            || c.processed_path != default.processed_path
            || c.ambient_sweep_schedule != default.ambient_sweep_schedule;
        if unimplemented_fields_differ {
            tracing::warn!(
                "[tuning.capture] contains unimplemented settings — inbox_ignore_globs, \
                 inbox_path, capture_paths, ready_flag, and hold_flag are live, but settle \
                 windows, processed_path, and the ambient sweep all do nothing. \
                 The vault watcher runs; the inbox layer above it is not built."
            );
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
            // `domain` may be absent (a chat profile has no pack) but must not be *present and
            // blank* — that reads as an unfinished edit rather than a deliberate omission.
            if profile.domain.as_ref().is_some_and(|d| d.trim().is_empty()) {
                return Err(Error::Config(format!(
                    "topology.session_profiles['{}'].domain is empty — name a domain pack \
                     (e.g. \"life\", \"coding\") or omit the key entirely for a chat-only profile",
                    profile.name
                )));
            }
            if !profile.enabled {
                continue;
            }

            // Two shapes, and mixing them means two answers to "what may this do".
            if profile.declares_authority() && profile.component.is_some() {
                return Err(Error::Config(format!(
                    "topology.session_profiles['{}'] sets both `component` and its own \
                     mcps/read/write — pick one: `component` borrows a policy grant wholesale, \
                     while declaring authority states it here (use `ceiling` to bound it)",
                    profile.name
                )));
            }

            if profile.declares_authority() {
                // A ceiling that names nothing would narrow every declaration to nothing, so the
                // profile would silently grant no tools — the worst failure this feature can have,
                // and indistinguishable at runtime from a profile that meant to grant nothing.
                if let Some(ceiling) = &profile.ceiling
                    && !self.policy.grants.iter().any(|g| &g.component == ceiling)
                {
                    return Err(Error::Config(format!(
                        "topology.session_profiles['{}'].ceiling '{ceiling}' names no policy.toml \
                         [[grants]] entry — every declared capability would narrow to nothing",
                        profile.name
                    )));
                }
                for entry in &profile.mcps {
                    let mcp = entry.mcp_name();
                    if !self.topology.mcps.iter().any(|m| m.name == mcp) {
                        return Err(Error::Config(format!(
                            "topology.session_profiles['{}'].mcps names unknown MCP '{mcp}' \
                             (not in topology.mcps)",
                            profile.name
                        )));
                    }
                    if let McpGrant::Narrowed { tools, .. } = entry
                        && tools.is_empty()
                    {
                        return Err(Error::Config(format!(
                            "topology.session_profiles['{}'].mcps entry for '{mcp}' lists no \
                             tools — grants nothing. Write \"{mcp}\" as a bare string for the \
                             whole server, or name the tools.",
                            profile.name
                        )));
                    }
                }
                for zone in profile.read.iter().chain(profile.write.iter()) {
                    if !self.policy.zones.iter().any(|z| &z.zone == zone) {
                        return Err(Error::Config(format!(
                            "topology.session_profiles['{}'] references undeclared zone '{zone}'",
                            profile.name
                        )));
                    }
                }
            } else {
                let component = profile.component_key();
                if !self.policy.grants.iter().any(|g| g.component == component) {
                    return Err(Error::Config(format!(
                        "topology.session_profiles['{}'].component '{component}' names no \
                         policy.toml [[grants]] entry — the session would run with zero authority",
                        profile.name
                    )));
                }
            }
        }

        // Schedules that name a profile must match an *enabled* session profile — same fail-fast
        // style as pool names (daemon fail-closed is the runtime backstop; load-time catches typos).
        let profile_exists = |name: &str| {
            self.topology
                .session_profiles
                .iter()
                .any(|p| p.enabled && p.name == name)
        };
        for schedule in &self.topology.schedules {
            if let Some(profile) = &schedule.profile
                && !profile_exists(profile)
            {
                return Err(Error::Config(format!(
                    "topology.schedules['{}'].profile '{profile}' does not name an enabled \
                     topology.session_profiles entry",
                    schedule.name
                )));
            }
        }
        // Hooks share the same profile stamp as schedules — same fail-fast rule.
        for hook in &self.topology.hooks {
            if let Some(profile) = &hook.profile
                && !profile_exists(profile)
            {
                return Err(Error::Config(format!(
                    "topology.hooks['{}'].profile '{profile}' does not name an enabled \
                     topology.session_profiles entry",
                    hook.name
                )));
            }
        }

        // Coding projects (S3/G4): unique names, non-empty absolute roots.
        let mut seen_project_names = std::collections::HashSet::new();
        for project in &self.topology.projects {
            if project.name.trim().is_empty() {
                return Err(Error::Config(
                    "topology.projects entry has an empty name".into(),
                ));
            }
            if !seen_project_names.insert(project.name.as_str()) {
                return Err(Error::Config(format!(
                    "topology.projects has a duplicate name '{}'",
                    project.name
                )));
            }
            if project.root.as_os_str().is_empty() {
                return Err(Error::Config(format!(
                    "topology.projects['{}'].root is empty",
                    project.name
                )));
            }
            if !project.root.is_absolute() {
                return Err(Error::Config(format!(
                    "topology.projects['{}'].root must be an absolute path (got '{}')",
                    project.name,
                    project.root.display()
                )));
            }
        }

        Ok(())
    }
}

/// Result of authorizing a coding goal's workspace (G4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingWorkspaceAuth {
    /// No project/path requested — the coding pack may use its ephemeral temp workspace.
    Ephemeral,
    /// Operator-authorized project; `root` is the resolved absolute workspace path.
    Project { name: String, root: PathBuf },
}

/// Why a coding workspace was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingAuthError {
    UnknownProject {
        name: String,
    },
    UndeclaredWorkspace {
        path: String,
    },
    NotWritable {
        name: String,
        write_class: WriteClass,
    },
    InvalidPath {
        reason: String,
    },
}

impl std::fmt::Display for CodingAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProject { name } => write!(
                f,
                "unknown coding project '{name}' — declare it under topology.toml [[projects]] \
                 or pick a name from GET /api/projects"
            ),
            Self::UndeclaredWorkspace { path } => write!(
                f,
                "workspace '{path}' is not under any declared [[projects]] root — undeclared \
                 paths are refused (fail-closed)"
            ),
            Self::NotWritable { name, write_class } => write!(
                f,
                "project '{name}' has write_class={write_class:?}, which does not allow direct \
                 agent writes — coding sessions require agent_writable (or shared)"
            ),
            Self::InvalidPath { reason } => write!(f, "invalid coding workspace path: {reason}"),
        }
    }
}

impl std::error::Error for CodingAuthError {}

/// Canonicalize an existing directory, or error with a stable reason string.
///
/// Callers that may receive non-canonical spellings (client `workspace_root`) must go through
/// [`resolve_workspace_path`] so Phase-1 openability runs first. Project roots from operator
/// config are already absolute and normally already canonical.
fn canonicalize_existing_dir(path: &Path) -> std::result::Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(format!("path is not a directory: {}", path.display()));
    }
    std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Collapse `.` / `..` components without consulting the filesystem.
///
/// **Not a security boundary.** Lexical cleanup exists so Windows can open paths that still
/// contain `..` under a verbatim (`\\?\`) prefix — Win32 does not process those components for
/// verbatim paths, so `exists`/`canonicalize` would fail before Phase 2 can run. Authorization
/// must use only the result of [`canonicalize_existing_dir`] (symlinks resolved).
fn lexically_normalize(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                match out.last() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    // Absolute path at drive/root: extra `..` is a no-op (same as GetFullPathName).
                    Some(Component::Prefix(_) | Component::RootDir) => {}
                    // Relative path still above the starting point — keep the `..`.
                    _ => out.push(c),
                }
            }
            Component::Normal(_) => out.push(c),
        }
    }
    out.iter().collect()
}

/// Phase 1: absolute path the OS can open. Collapses `.`/`..`; does **not** resolve symlinks.
///
/// Security decisions use Phase 2 ([`canonicalize_existing_dir`]) only.
fn make_openable_absolute(path: &Path) -> std::result::Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        // std::path::absolute processes `.`/`..` for non-verbatim paths via the OS; verbatim
        // inputs are returned as-is, so lexical normalize still runs below.
        std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?
    };
    Ok(lexically_normalize(&absolute))
}

/// Resolve an untrusted workspace path to an existing absolute directory.
///
/// 1. **Phase 1** — [`make_openable_absolute`]: make the path openable (esp. Windows `\\?\` + `..`).
/// 2. **Phase 2** — [`canonicalize_existing_dir`]: filesystem truth for authorization.
///
/// Missing paths are rejected (coding sessions need a real checkout, not a future directory).
fn resolve_workspace_path(path: &Path) -> std::result::Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("path is empty".into());
    }
    let openable = make_openable_absolute(path)?;
    canonicalize_existing_dir(&openable)
}

/// True when `candidate` is `root` or a strict subdirectory of `root` (after both are absolute).
fn path_is_within(root: &Path, candidate: &Path) -> bool {
    if candidate == root {
        return true;
    }
    let mut root_components = root.components().peekable();
    let mut cand_components = candidate.components().peekable();
    // Reject `..` escapes in either path after canonicalization should already have removed them;
    // still refuse if present so a non-canonical fallback cannot walk out.
    if root.components().any(|c| matches!(c, Component::ParentDir))
        || candidate
            .components()
            .any(|c| matches!(c, Component::ParentDir))
    {
        return false;
    }
    loop {
        match (root_components.next(), cand_components.next()) {
            (Some(r), Some(c)) if r == c => continue,
            (None, Some(_)) => return true, // candidate longer → under root
            (None, None) => return true,    // equal
            _ => return false,
        }
    }
}

#[cfg(test)]
mod project_auth_tests {
    use super::*;
    use liberado_common::WriteClass;
    use std::fs;

    fn cfg_with_projects(projects: Vec<ProjectConfig>) -> Config {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/tmp/vault");
        cfg.topology.projects = projects;
        cfg
    }

    fn temp_project(name: &str, write_class: WriteClass) -> (tempfile::TempDir, ProjectConfig) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let project = ProjectConfig {
            name: name.into(),
            root: root.clone(),
            write_class,
            enabled: true,
            preflight: Default::default(),
        };
        (dir, project)
    }

    #[test]
    fn ephemeral_when_neither_project_nor_path() {
        let cfg = cfg_with_projects(vec![]);
        assert_eq!(
            cfg.authorize_coding_workspace(None, None).unwrap(),
            CodingWorkspaceAuth::Ephemeral
        );
    }

    #[test]
    fn named_project_resolves_to_root() {
        let (_dir, project) = temp_project("liberado", WriteClass::AgentWritable);
        let root = project.root.clone();
        let cfg = cfg_with_projects(vec![project]);
        match cfg
            .authorize_coding_workspace(Some("liberado"), None)
            .unwrap()
        {
            CodingWorkspaceAuth::Project { name, root: r } => {
                assert_eq!(name, "liberado");
                assert_eq!(r, root);
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn unknown_project_name_is_refused() {
        let cfg = cfg_with_projects(vec![]);
        let err = cfg
            .authorize_coding_workspace(Some("nope"), None)
            .unwrap_err();
        assert!(matches!(err, CodingAuthError::UnknownProject { .. }));
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn undeclared_workspace_path_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        let outside = fs::canonicalize(outside.path()).unwrap();
        let (_dir, project) = temp_project("liberado", WriteClass::AgentWritable);
        let cfg = cfg_with_projects(vec![project]);
        let err = cfg
            .authorize_coding_workspace(None, Some(outside.to_str().unwrap()))
            .unwrap_err();
        assert!(matches!(err, CodingAuthError::UndeclaredWorkspace { .. }));
    }

    #[test]
    fn workspace_under_project_root_is_allowed() {
        let (_dir, project) = temp_project("liberado", WriteClass::AgentWritable);
        let sub = project.root.join("crates");
        fs::create_dir_all(&sub).unwrap();
        let sub = fs::canonicalize(&sub).unwrap();
        let cfg = cfg_with_projects(vec![project]);
        match cfg
            .authorize_coding_workspace(None, Some(sub.to_str().unwrap()))
            .unwrap()
        {
            CodingWorkspaceAuth::Project { name, root } => {
                assert_eq!(name, "liberado");
                assert_eq!(root, sub);
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn proposal_only_project_refuses_coding() {
        let (_dir, project) = temp_project("docs", WriteClass::ProposalOnly);
        let cfg = cfg_with_projects(vec![project]);
        let err = cfg
            .authorize_coding_workspace(Some("docs"), None)
            .unwrap_err();
        assert!(matches!(err, CodingAuthError::NotWritable { .. }));
    }

    #[test]
    fn proposal_only_project_refuses_path_only_coding() {
        let (_dir, project) = temp_project("docs", WriteClass::ProposalOnly);
        let sub = project.root.join("pages");
        std::fs::create_dir_all(&sub).unwrap();
        let sub = std::fs::canonicalize(&sub).unwrap();
        let cfg = cfg_with_projects(vec![project]);
        let err = cfg
            .authorize_coding_workspace(None, Some(sub.to_str().unwrap()))
            .unwrap_err();
        assert!(matches!(err, CodingAuthError::NotWritable { .. }));
    }

    #[test]
    fn disabled_project_is_not_found() {
        let (_dir, mut project) = temp_project("liberado", WriteClass::AgentWritable);
        project.enabled = false;
        let cfg = cfg_with_projects(vec![project]);
        assert!(matches!(
            cfg.authorize_coding_workspace(Some("liberado"), None),
            Err(CodingAuthError::UnknownProject { .. })
        ));
    }

    #[test]
    fn validate_rejects_duplicate_project_names() {
        let (_a, p1) = temp_project("same", WriteClass::AgentWritable);
        let (_b, mut p2) = temp_project("same", WriteClass::AgentWritable);
        p2.name = "same".into();
        let cfg = cfg_with_projects(vec![p1, p2]);
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("duplicate name"), "got {err}");
    }

    #[test]
    fn validate_rejects_relative_project_root() {
        let mut cfg = Config::default();
        cfg.topology.vault_path = PathBuf::from("/tmp/vault");
        cfg.topology.projects.push(ProjectConfig {
            name: "rel".into(),
            root: PathBuf::from("relative/path"),
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        });
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("absolute"), "got {err}");
    }

    #[test]
    fn most_specific_project_wins_over_broad_non_writable() {
        // broad proposal_only umbrella + narrow agent_writable sub-project
        let umbrella = tempfile::tempdir().unwrap();
        let umbrella_root = std::fs::canonicalize(umbrella.path()).unwrap();
        let nested = umbrella_root.join("life-os");
        std::fs::create_dir_all(&nested).unwrap();
        let cfg = cfg_with_projects(vec![
            ProjectConfig {
                name: "umbrella".into(),
                root: umbrella_root.clone(),
                write_class: WriteClass::ProposalOnly,
                enabled: true,
                preflight: Default::default(),
            },
            ProjectConfig {
                name: "life-os".into(),
                root: nested.clone(),
                write_class: WriteClass::AgentWritable,
                enabled: true,
                preflight: Default::default(),
            },
        ]);
        match cfg
            .authorize_coding_workspace(None, Some(nested.to_str().unwrap()))
            .unwrap()
        {
            CodingWorkspaceAuth::Project { name, .. } => {
                assert_eq!(name, "life-os", "narrower writable project must win");
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn scenic_workspace_path_with_parent_components_is_resolved() {
        // Built as a *string*: PathBuf::join would collapse `..` on Windows verbatim roots
        // before we ever see it, so the bug (exists fails on `\\?\…\crates\..\crates`) never
        // appears. Authorization must still accept the scenic spelling and return the canonical
        // child.
        let (_dir, project) = temp_project("liberado", WriteClass::AgentWritable);
        let sub = project.root.join("crates");
        fs::create_dir_all(&sub).unwrap();
        let sub = fs::canonicalize(&sub).unwrap();
        let sep = std::path::MAIN_SEPARATOR;
        let scenic = format!("{}{sep}crates{sep}..{sep}crates", project.root.display());
        let cfg = cfg_with_projects(vec![project]);
        match cfg.authorize_coding_workspace(None, Some(&scenic)).unwrap() {
            CodingWorkspaceAuth::Project { name, root } => {
                assert_eq!(name, "liberado");
                assert_eq!(root, sub, "pack-facing root must be canonical, not scenic");
            }
            other => panic!("expected Project, got {other:?}"),
        }
    }

    #[test]
    fn scenic_path_that_escapes_every_project_is_refused() {
        // `project_root/../<sibling>` resolves outside every declared root.
        // Built as a string so Windows verbatim roots keep the ParentDir component.
        let (_dir, project) = temp_project("liberado", WriteClass::AgentWritable);
        let parent = project.root.parent().expect("canonical temp has a parent");
        let sibling = tempfile::Builder::new()
            .prefix("escape-")
            .tempdir_in(parent)
            .unwrap();
        let sibling_name = sibling.path().file_name().unwrap().to_string_lossy();
        let sep = std::path::MAIN_SEPARATOR;
        let scenic = format!("{}{sep}..{sep}{sibling_name}", project.root.display());
        let cfg = cfg_with_projects(vec![project]);
        let err = cfg
            .authorize_coding_workspace(None, Some(&scenic))
            .unwrap_err();
        assert!(
            matches!(err, CodingAuthError::UndeclaredWorkspace { .. }),
            "escape via .. must be refused after resolve, got {err:?}"
        );
    }

    #[test]
    fn missing_workspace_path_is_invalid() {
        let (_dir, project) = temp_project("liberado", WriteClass::AgentWritable);
        let missing = project.root.join("does-not-exist");
        let cfg = cfg_with_projects(vec![project]);
        let err = cfg
            .authorize_coding_workspace(None, Some(&missing.to_string_lossy()))
            .unwrap_err();
        assert!(
            matches!(err, CodingAuthError::InvalidPath { .. }),
            "missing checkout must be InvalidPath, got {err:?}"
        );
    }

    #[test]
    fn empty_workspace_path_is_treated_as_absent() {
        let cfg = cfg_with_projects(vec![]);
        assert_eq!(
            cfg.authorize_coding_workspace(None, Some("   ")).unwrap(),
            CodingWorkspaceAuth::Ephemeral
        );
    }

    #[test]
    fn builder_returns_default_builder() {
        let b = Config::builder();
        let _built = b.build();
        // The builder with all defaults may fail validation; that's fine — we just
        // test that the constructor itself doesn't panic.
    }

    #[test]
    fn enabled_session_profiles_filters_disabled() {
        use crate::model::topology::SessionProfile;
        let mut cfg = cfg_with_projects(vec![]);
        cfg.topology.session_profiles = vec![
            SessionProfile {
                name: "a".into(),
                enabled: true,
                ..SessionProfile::empty("a")
            },
            SessionProfile {
                name: "b".into(),
                enabled: false,
                ..SessionProfile::empty("b")
            },
        ];
        let enabled = cfg.enabled_session_profiles();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].name, "a");
    }

    #[test]
    fn path_is_within_rejects_different_root() {
        // root and candidate are different trees
        assert!(!super::path_is_within(
            Path::new("/C:/a"),
            Path::new("/C:/b/sub")
        ));
    }

    #[test]
    fn path_is_within_accepts_genuine_child() {
        // /C:/a/b/c is within /C:/a
        assert!(super::path_is_within(
            Path::new("/C:/a"),
            Path::new("/C:/a/b/c")
        ));
    }

    #[test]
    fn lexically_normalize_removes_dot_dot() {
        let result = super::lexically_normalize(Path::new("/a/b/../c"));
        assert_eq!(result, PathBuf::from("/a/c"));
    }

    #[test]
    fn lexically_normalize_preserves_root() {
        let result = super::lexically_normalize(Path::new("/a/b/c"));
        assert_eq!(result, PathBuf::from("/a/b/c"));
    }

    #[test]
    fn path_is_within_rejects_sibling() {
        let a = PathBuf::from("/tmp/projects/a");
        let b = PathBuf::from("/tmp/projects/b");
        assert!(!path_is_within(&a, &b));
        assert!(path_is_within(&a, &a));
        assert!(path_is_within(&a, &a.join("src")));
    }

    #[test]
    fn a_sibling_whose_name_merely_starts_with_the_root_is_not_inside_it() {
        // The failure this boundary is most often written with: `starts_with` on the string form
        // says `/tmp/projects/app-evil` is under `/tmp/projects/app`, because it is — as text.
        // Comparing components is what makes it not so, and nothing here pinned that.
        let root = PathBuf::from("/tmp/projects/app");
        for outside in [
            "/tmp/projects/app-evil",
            "/tmp/projects/app2",
            "/tmp/projects/appendix/src",
        ] {
            assert!(
                !path_is_within(&root, Path::new(outside)),
                "{outside} shares a textual prefix with the root but is not under it"
            );
        }
        assert!(
            path_is_within(&root, Path::new("/tmp/projects/app/src")),
            "a genuine child must still be allowed"
        );
    }

    #[test]
    fn an_authorized_project_refuses_a_prefix_sibling_end_to_end() {
        // The unit above is about the predicate; this is about the answer the daemon acts on.
        let parent = tempfile::tempdir().unwrap();
        let parent = std::fs::canonicalize(parent.path()).unwrap();
        let root = parent.join("app");
        let sibling = parent.join("app-evil");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        let cfg = cfg_with_projects(vec![ProjectConfig {
            name: "app".into(),
            root: root.clone(),
            write_class: WriteClass::AgentWritable,
            enabled: true,
            preflight: Default::default(),
        }]);

        let err = cfg
            .authorize_coding_workspace(Some("app"), Some(&sibling.to_string_lossy()))
            .unwrap_err();
        assert!(
            matches!(err, CodingAuthError::UndeclaredWorkspace { .. }),
            "a prefix sibling must be refused, got {err:?}"
        );
        // …and the same path with no project named, so neither branch is the lenient one.
        let err = cfg
            .authorize_coding_workspace(None, Some(&sibling.to_string_lossy()))
            .unwrap_err();
        assert!(
            matches!(err, CodingAuthError::UndeclaredWorkspace { .. }),
            "path-only auth must refuse it too, got {err:?}"
        );
    }
}
