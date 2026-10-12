//! Vault-wide grant predicate.
//!
//! Lives beside [`crate::capability::CapabilitySet`] so the two new questions
//! (`holds_any_zone_write`, `holds_vault_wide_grant`) do not grow that file.
//! The vault-wide gate asks only [`VaultWideGrant::holds_vault_wide_grant`].

use crate::capability::{Capability, CapabilitySet, Zone};

/// Grant questions the vault-write gate asks.
///
/// Inherent methods would have to live in `capability.rs`. A trait keeps that
/// file at its module-health baseline while callers still write
/// `caps.holds_vault_wide_grant(marker)` once this trait is in scope.
pub trait VaultWideGrant {
    /// Whether this set holds **any** `Write(Zone)` capability.
    ///
    /// Reports and dashboards use the coarse question. The vault-wide gate does
    /// not: `Write(Vault("Tasks"))` authorises rearranging Tasks, not picking the
    /// active vault for the whole process.
    fn holds_any_zone_write(&self) -> bool;

    /// Whether this set holds the explicit vault-wide marker grant.
    ///
    /// A vault-wide tool (`begin_fanout`, `commit_fanout`, `abandon_fanout`,
    /// `vector_reindex`) asks this. `Write(Vault("Tasks"))` says the session may
    /// mutate Tasks. It does not say the session may rebuild the search index.
    /// Those affect every concurrent session. The earlier "any zone write
    /// authorises vault-wide" model was too weak. The grant opts in with
    /// `Capability::Write(Zone::vault(VAULT_WIDE_ZONE_MARKER))`. The runtime
    /// writes that same capability when the human picks Once, Session, or
    /// Everywhere on the permission card.
    fn holds_vault_wide_grant(&self, marker: &str) -> bool;
}

impl VaultWideGrant for CapabilitySet {
    fn holds_any_zone_write(&self) -> bool {
        self.capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::Write(_)))
    }

    fn holds_vault_wide_grant(&self, marker: &str) -> bool {
        self.capabilities
            .iter()
            .any(|cap| write_zone_name(cap) == Some(marker))
    }
}

fn write_zone_name(cap: &Capability) -> Option<&str> {
    match cap {
        Capability::Write(Zone::Vault(name) | Zone::Named(name)) => Some(name.as_str()),
        _ => None,
    }
}
