//! Live session-profile names and chat-access masks.
//!
//! `[[session_profiles]]` in `topology.toml` stays the operator file. This module does not
//! rewrite it. A rename or delete is a ledger under the data directory, plus a conversation
//! grant update through [`ChatSessions::set_profile`](liberado_main_agent::ChatSessions::set_profile)
//! — the same header write a profiled create uses.
//!
//! Chat plugin switches record which MCP servers the face may call. They cannot grant a server
//! `policy.toml` did not grant to `main-agent`. The dispatcher grant is unchanged.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use liberado_bootstrap::Config;
use liberado_common::Error;
use liberado_config::{BUILTIN_AUTOMATION_EDITOR_PROFILE, ResolvedProfile};

/// Named chat profile the picker treats as the built-in default. Not the unnamed grant.
pub const BUILTIN_DEFAULT_PROFILE: &str = "chat-default";

const LEDGER_FILE: &str = "session-profile-ledger.json";
const PLUGIN_FILE: &str = "plugin-chat-access.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileReject {
    Unknown,
    Protected,
    Collision,
    Invalid(&'static str),
}

impl ProfileReject {
    pub fn message(&self, name: &str) -> String {
        match self {
            Self::Unknown => format!(
                "session profile '{name}' does not name an enabled topology.session_profiles entry"
            ),
            Self::Protected => format!(
                "session profile '{name}' is built in (default or Agent Creator) and cannot be changed"
            ),
            Self::Collision => format!("session profile '{name}' already exists"),
            Self::Invalid(reason) => (*reason).to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RenamePlan {
    Unchanged,
    Apply { original: String },
}

#[derive(Debug, PartialEq, Eq)]
pub struct DeletePlan {
    pub original: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginReject {
    NotGranted,
    Store(String),
}

impl PluginReject {
    pub fn message(&self, name: &str) -> String {
        match self {
            Self::NotGranted => {
                format!("policy does not grant '{name}' to chat; this switch cannot widen access")
            }
            Self::Store(error) => error.clone(),
        }
    }
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct ProfileLedger {
    #[serde(default)]
    renamed: BTreeMap<String, String>,
    #[serde(default)]
    deleted: BTreeSet<String>,
}

struct ProfileState {
    ledger: ProfileLedger,
    path: Option<PathBuf>,
}

/// Live names over the boot topology. Clone shares the ledger.
#[derive(Clone)]
pub struct ProfileCatalog {
    inner: Arc<Mutex<ProfileState>>,
    config: Arc<Config>,
}

impl ProfileCatalog {
    /// No ledger file. Mutations stay in memory. Test and inert app states use this.
    #[cfg(test)]
    pub fn memory(config: Arc<Config>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ProfileState {
                ledger: ProfileLedger::default(),
                path: None,
            })),
            config,
        }
    }

    /// Load the data-dir ledger. A missing file is an empty ledger. A file that does not
    /// parse is ignored, the same fail-safe as the grants overlay.
    pub fn open(config: Arc<Config>, data_dir: &Path) -> Self {
        let path = data_dir.join(LEDGER_FILE);
        let ledger = read_json(&path).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "session-profile ledger ignored");
            ProfileLedger::default()
        });
        Self {
            inner: Arc::new(Mutex::new(ProfileState {
                ledger,
                path: Some(path),
            })),
            config,
        }
    }

    pub fn resolve(
        &self,
        profile: Option<&str>,
        domain_fallback: &str,
    ) -> Result<ResolvedProfile, Error> {
        let Some(name) = profile else {
            return self.config.resolve_session_profile(None, domain_fallback);
        };
        let original = {
            let guard = self.lock();
            match original_of(&self.config, &guard.ledger, name) {
                Some(original) => original,
                None => return profile_absent_from_ledger(self, name, domain_fallback),
            }
        };
        let mut resolved = self
            .config
            .resolve_session_profile(Some(&original), domain_fallback)?;
        if resolved.name.as_deref() != Some(name) {
            resolved.name = Some(name.to_string());
        }
        Ok(resolved)
    }

    /// `GET /api/profiles` body. `agent_eligible` follows the topology name, which is what
    /// `[chat] agent_profiles` lists.
    pub fn list_json(&self) -> serde_json::Value {
        let guard = self.lock();
        let eligible: HashSet<&str> = self
            .config
            .tuning
            .chat
            .agent_profiles
            .iter()
            .map(String::as_str)
            .collect();
        let profiles: Vec<serde_json::Value> = self
            .config
            .enabled_session_profiles()
            .into_iter()
            .filter(|profile| !guard.ledger.deleted.contains(&profile.name))
            .map(|profile| {
                let name = guard
                    .ledger
                    .renamed
                    .get(&profile.name)
                    .cloned()
                    .unwrap_or_else(|| profile.name.clone());
                serde_json::json!({
                    "name": name,
                    "description": profile.description,
                    "domain": profile.domain,
                    "delegation": profile.delegation,
                    "model": profile.model,
                    "agent_eligible": eligible.contains(profile.name.as_str()),
                })
            })
            .collect();
        serde_json::json!({ "profiles": profiles })
    }

    pub fn plan_rename(&self, from: &str, to_raw: &str) -> Result<RenamePlan, ProfileReject> {
        let to = validate_profile_name(to_raw)?;
        let guard = self.lock();
        let Some(original) = original_of(&self.config, &guard.ledger, from) else {
            return Err(missing_profile_reject(from));
        };
        if is_protected(&original) || is_protected(from) || is_protected(&to) {
            return Err(ProfileReject::Protected);
        }
        if to == from {
            return Ok(RenamePlan::Unchanged);
        }
        if name_taken(&self.config, &guard.ledger, &to, &original) {
            return Err(ProfileReject::Collision);
        }
        Ok(RenamePlan::Apply { original })
    }

    pub fn commit_rename(&self, original: &str, to: &str) -> Result<(), String> {
        let mut guard = self.lock();
        if to == original {
            guard.ledger.renamed.remove(original);
        } else {
            guard
                .ledger
                .renamed
                .insert(original.to_string(), to.to_string());
        }
        persist_profile(&guard)
    }

    pub fn plan_delete(&self, name: &str) -> Result<DeletePlan, ProfileReject> {
        let guard = self.lock();
        let Some(original) = original_of(&self.config, &guard.ledger, name) else {
            return Err(missing_profile_reject(name));
        };
        if is_protected(&original) || is_protected(name) {
            return Err(ProfileReject::Protected);
        }
        Ok(DeletePlan { original })
    }

    pub fn commit_delete(&self, original: &str) -> Result<(), String> {
        let mut guard = self.lock();
        guard.ledger.deleted.insert(original.to_string());
        guard.ledger.renamed.remove(original);
        persist_profile(&guard)
    }

    /// Undo [`commit_delete`](Self::commit_delete) when conversation rebind fails.
    pub fn revert_delete(&self, original: &str) {
        let mut guard = self.lock();
        guard.ledger.deleted.remove(original);
        if let Err(error) = persist_profile(&guard) {
            tracing::warn!(%error, "session-profile ledger revert was not saved");
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProfileState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn persist_profile(state: &ProfileState) -> Result<(), String> {
    let Some(path) = &state.path else {
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(&state.ledger).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes).map_err(|e| e.to_string())
}

fn original_of(config: &Config, ledger: &ProfileLedger, live: &str) -> Option<String> {
    config
        .enabled_session_profiles()
        .into_iter()
        .find_map(|profile| {
            if ledger.deleted.contains(&profile.name) {
                return None;
            }
            let current = ledger
                .renamed
                .get(&profile.name)
                .map(String::as_str)
                .unwrap_or(profile.name.as_str());
            (current == live).then(|| profile.name.clone())
        })
}

fn name_taken(config: &Config, ledger: &ProfileLedger, live: &str, except_original: &str) -> bool {
    config
        .enabled_session_profiles()
        .into_iter()
        .any(|profile| {
            if profile.name == except_original || ledger.deleted.contains(&profile.name) {
                return false;
            }
            let current = ledger
                .renamed
                .get(&profile.name)
                .map(String::as_str)
                .unwrap_or(profile.name.as_str());
            current == live
        })
}

pub fn is_protected(name: &str) -> bool {
    name == BUILTIN_DEFAULT_PROFILE
        || name == BUILTIN_AUTOMATION_EDITOR_PROFILE
        || liberado_conversation_store::is_agent_creator_profile(Some(name))
}

/// A live name the ledger does not currently publish.
///
/// A topology row that was deleted or renamed stays unknown, so the catalog
/// does not fall through to the boot entry. The built-in editor name is
/// protected even when topology never listed it.
fn missing_profile_reject(name: &str) -> ProfileReject {
    if is_protected(name) {
        ProfileReject::Protected
    } else {
        ProfileReject::Unknown
    }
}

/// No ledger row for `name`. A deleted topology profile stays an error.
/// A name topology never listed goes to [`Config::resolve_session_profile`],
/// which accepts the built-in editor hat and rejects everything else.
fn profile_absent_from_ledger(
    catalog: &ProfileCatalog,
    name: &str,
    domain_fallback: &str,
) -> Result<ResolvedProfile, Error> {
    if catalog
        .config
        .topology
        .session_profiles
        .iter()
        .any(|profile| profile.name == name)
    {
        return Err(unknown_profile_error(name));
    }
    catalog
        .config
        .resolve_session_profile(Some(name), domain_fallback)
}

fn unknown_profile_error(name: &str) -> Error {
    Error::Config(format!(
        "session profile '{name}' does not name an enabled \
         topology.session_profiles entry"
    ))
}

fn validate_profile_name(raw: &str) -> Result<String, ProfileReject> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ProfileReject::Invalid("Enter a profile name."));
    }
    if name.chars().count() > 64 {
        return Err(ProfileReject::Invalid(
            "Keep the profile name under 64 characters.",
        ));
    }
    let ok = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !ok {
        return Err(ProfileReject::Invalid(
            "Use letters, digits, and . _ - in the profile name.",
        ));
    }
    Ok(name.to_string())
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct PluginLedger {
    #[serde(default)]
    disabled: BTreeSet<String>,
}

struct PluginState {
    ledger: PluginLedger,
    path: Option<PathBuf>,
}

/// Which MCP servers the chat face may call, narrowed from the `main-agent` grant.
#[derive(Clone)]
pub struct PluginAccess {
    inner: Arc<Mutex<PluginState>>,
}

impl PluginAccess {
    #[cfg(test)]
    pub fn memory() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PluginState {
                ledger: PluginLedger::default(),
                path: None,
            })),
        }
    }

    pub fn open(data_dir: &Path) -> Self {
        let path = data_dir.join(PLUGIN_FILE);
        let ledger = read_json(&path).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "plugin chat-access ledger ignored");
            PluginLedger::default()
        });
        Self {
            inner: Arc::new(Mutex::new(PluginState {
                ledger,
                path: Some(path),
            })),
        }
    }

    /// Policy grant AND not switched off. Never true when policy does not grant the server.
    pub fn chat_visible(&self, name: &str, policy_grants: bool) -> bool {
        if !policy_grants {
            return false;
        }
        let guard = self.lock();
        !guard.ledger.disabled.contains(name)
    }

    pub fn set_chat_enabled(
        &self,
        name: &str,
        enabled: bool,
        policy_grants: bool,
    ) -> Result<bool, PluginReject> {
        if enabled && !policy_grants {
            return Err(PluginReject::NotGranted);
        }
        let mut guard = self.lock();
        if enabled {
            guard.ledger.disabled.remove(name);
        } else {
            guard.ledger.disabled.insert(name.to_string());
        }
        persist_plugins(&guard).map_err(PluginReject::Store)?;
        Ok(policy_grants && !guard.ledger.disabled.contains(name))
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.lock().ledger.disabled.contains(name)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PluginState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn persist_plugins(state: &PluginState) -> Result<(), String> {
    let Some(path) = &state.path else {
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(&state.ledger).map_err(|e| e.to_string())?;
    write_atomic(path, &bytes).map_err(|e| e.to_string())
}

/// Hide a switched-off MCP from the chat face runtime. Built-in tools have no `mcp:` prefix
/// and stay. This does not change the dispatcher runtime.
pub fn mask_chat_runtime(
    inner: Arc<dyn liberado_executor::ToolRuntime>,
    access: PluginAccess,
) -> Arc<dyn liberado_executor::ToolRuntime> {
    let filter_access = access.clone();
    let gate_access = access;
    Arc::new(liberado_executor::DecoratingRuntime::with_filter_and_gate(
        inner,
        move |tool| !filter_access.is_disabled(liberado_common::mcp_of(&tool.name)),
        move |call| {
            let mcp = liberado_common::mcp_of(&call.name);
            if gate_access.is_disabled(mcp) {
                Some(Err(format!("chat access to MCP '{mcp}' is off")))
            } else {
                None
            }
        },
    ))
}

/// Profiles plus plugin switches for one process.
#[derive(Clone)]
pub struct LiveSurfaces {
    pub profiles: ProfileCatalog,
    pub plugins: PluginAccess,
}

impl LiveSurfaces {
    #[cfg(test)]
    pub fn inert() -> Self {
        Self::memory(Arc::new(Config::default()))
    }

    /// In-memory ledger over `config`. Use this when a test's `AppState.config` must resolve.
    #[cfg(test)]
    pub fn memory(config: Arc<Config>) -> Self {
        Self {
            profiles: ProfileCatalog::memory(config),
            plugins: PluginAccess::memory(),
        }
    }

    pub fn open(config: Arc<Config>, data_dir: &Path) -> Self {
        Self {
            profiles: ProfileCatalog::open(Arc::clone(&config), data_dir),
            plugins: PluginAccess::open(data_dir),
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e.to_string()),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
#[path = "surfaces_tests.rs"]
mod tests;
