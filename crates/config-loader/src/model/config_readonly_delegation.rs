//! Resolution of an enabled `[[session_profiles]]` row into a [`ResolvedProfile`].
//!
//! It lives beside [`Config`] rather than in it because of the one rule that is not a copy of
//! the row: a read-only grant that says nothing about `delegation` resolves to off, instead of
//! inheriting the daemon default. Reading authority to decide authority is worth its own module.

use liberado_common::CapabilitySet;

use super::config::{Config, ResolvedProfile};
use super::topology::SessionProfile;

/// Everything a session needs to run under `profile`, which the caller has already matched as
/// enabled.
pub(super) fn declared_profile(config: &Config, profile: &SessionProfile) -> ResolvedProfile {
    let capabilities = config.profile_capabilities(profile);
    ResolvedProfile {
        name: Some(profile.name.clone()),
        description: profile.description.clone(),
        domain: profile.domain.clone(),
        overrides: profile.overrides.clone(),
        max_idle_secs: profile.max_idle_secs,
        delegation: read_only_delegation_default(profile.delegation, &capabilities),
        model: profile.model.clone(),
        prompt_append: profile.prompt_append.clone(),
        capabilities,
    }
}

/// `None` inherits the daemon default, except a read-only grant, which resolves to off.
///
/// An empty set is not read-only. An explicit `true` or `false` is kept.
fn read_only_delegation_default(
    declared: Option<bool>,
    capabilities: &CapabilitySet,
) -> Option<bool> {
    match declared {
        Some(value) => Some(value),
        None if capabilities.authorizes_only_reads() => Some(false),
        None => None,
    }
}

#[cfg(test)]
#[path = "config_readonly_delegation_tests.rs"]
mod tests;
