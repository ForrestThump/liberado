//! Built-in Automation Editor hat.
//!
//! Agent Creator runs as `operator`, which every deployment already lists.
//! The editor uses its own name. A node that never added that
//! `[[session_profiles]]` row must still resolve it. The grant is only the
//! `automation-editor` policy component. Delegation stays off. There is no
//! domain, so the hat is not a pack and not an Agents-shelf profile.
//!
//! A row of this name that exists and is disabled stays an error. Turning the
//! row off is an operator choice. Any other missing name stays an error.

use liberado_common::{Error, Result};

use super::config::{Config, ResolvedProfile};
use super::topology::empty_table;

/// Profile name the Automation Editor session stores on its grant.
pub const BUILTIN_AUTOMATION_EDITOR_PROFILE: &str = "automation-editor";

/// Called when no *enabled* `[[session_profiles]]` row matches `name`.
pub(super) fn profile_or_unknown(config: &Config, name: &str) -> Result<ResolvedProfile> {
    if topology_lists_profile_name(config, name) {
        return unknown_session_profile(name);
    }
    builtin_editor_profile(config, name)
}

fn topology_lists_profile_name(config: &Config, name: &str) -> bool {
    config
        .topology
        .session_profiles
        .iter()
        .any(|profile| profile.name == name)
}

fn builtin_editor_profile(config: &Config, name: &str) -> Result<ResolvedProfile> {
    if name != BUILTIN_AUTOMATION_EDITOR_PROFILE {
        return unknown_session_profile(name);
    }
    Ok(editor_profile_from_policy(config))
}

fn editor_profile_from_policy(config: &Config) -> ResolvedProfile {
    ResolvedProfile {
        name: Some(BUILTIN_AUTOMATION_EDITOR_PROFILE.to_string()),
        description: Some(
            "Built-in Automation Editor. Schedule tools only. Not on the Agents shelf.".into(),
        ),
        domain: None,
        capabilities: config
            .policy
            .capabilities_for(BUILTIN_AUTOMATION_EDITOR_PROFILE),
        overrides: empty_table(),
        max_idle_secs: None,
        delegation: Some(false),
        model: None,
        prompt_append: None,
    }
}

fn unknown_session_profile(name: &str) -> Result<ResolvedProfile> {
    Err(Error::Config(format!(
        "session profile '{name}' does not name an enabled \
         topology.session_profiles entry"
    )))
}

#[cfg(test)]
#[path = "config_builtin_editor_tests.rs"]
mod tests;
