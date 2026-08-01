//! Process-lifetime session grants — the in-memory half of the permission-expansion flow.
//!
//! When an agent hits a zone it wasn't granted, Liberado asks the human (Deny / Once / Session /
//! Everywhere). **Everywhere** persists to a file and survives a restart (see
//! `liberado_config::append_grant_to_overlay`); **Session** adds the capability *here* — an in-memory,
//! process-global store keyed by **pool name**, gone the moment the daemon restarts.
//!
//! ## Why a process global and not a threaded `Arc`
//!
//! A single pool has **two** independently-built [`Orchestrator`](../../liberado_orchestrator) instances
//! alive in the process: the *daemon's* (which runs `execute_approved` when the human taps a button)
//! and the *dispatch-pack's* (which runs the live chat-delegated work that will retry the blocked
//! write). They are constructed separately at boot from the same config, share no runtime state, and
//! neither can reach the other. A session grant tapped on the approval path (daemon) has to become
//! visible to the work path (pack) with no restart — and a process global keyed by pool name is the
//! one place both naturally see. Threading a shared `Arc<RwLock<..>>` through both boot paths would do
//! the same thing with more plumbing; this keeps the widening in one small, well-scoped place.
//!
//! The grant is applied **post-narrow** in `Orchestrator::run` (a human-authorized widening, exactly
//! like Everywhere widens the pool ceiling at boot), and downstream write-class guards still apply — a
//! session grant for a `human_only` zone is still downgraded to a proposal, never a silent direct write.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::capability::{Capability, CapabilitySet};

/// Pool-name → the capabilities a human granted "for this session" (process lifetime).
static GRANTS: OnceLock<RwLock<HashMap<String, CapabilitySet>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, CapabilitySet>> {
    GRANTS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Add `cap` to the process-lifetime session grant for `pool`. Idempotent — returns `true` only if
/// it was newly added (so callers can log a real change vs. a repeat approval).
pub fn grant_for_session(pool: &str, cap: Capability) -> bool {
    // A poisoned lock means some other thread panicked mid-mutation; the guarded data (a HashMap of
    // capability lists) is still structurally sound, so recover the inner value rather than cascade
    // the panic into an unrelated tool call.
    let mut map = store().write().unwrap_or_else(|e| e.into_inner());
    let set = map.entry(pool.to_string()).or_default();
    if set.contains(&cap) {
        return false;
    }
    set.grant(cap);
    true
}

/// The process-lifetime session grant for `pool` (an empty set if none) — folded post-narrow into the
/// effective ceiling by `Orchestrator::run`. Cloned on read; these sets are tiny.
pub fn session_grant(pool: &str) -> CapabilitySet {
    store()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(pool)
        .cloned()
        .unwrap_or_default()
}

/// Drop all session grants — a test helper (the store is process-global, so tests that assert on it
/// must reset it) and a plausible future "revoke all session grants" hook.
pub fn clear() {
    store().write().unwrap_or_else(|e| e.into_inner()).clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Zone;

    #[test]
    fn grant_is_readable_and_idempotent() {
        // Unique pool name: process-global store races under `cargo test` parallelism.
        let pool = format!("test-pool-grant-readable-{:?}", std::thread::current().id());
        let cap = Capability::Write(Zone::vault("sandbox"));

        assert!(session_grant(&pool).capabilities.is_empty());
        assert!(grant_for_session(&pool, cap.clone()), "first grant is new");
        assert!(session_grant(&pool).contains(&cap));
        // Repeat approval is a no-op, and doesn't duplicate.
        assert!(!grant_for_session(&pool, cap.clone()));
        assert_eq!(session_grant(&pool).capabilities.len(), 1);
    }

    #[test]
    fn grants_are_scoped_per_pool() {
        let id = format!("{:?}", std::thread::current().id());
        let pool_a = format!("pool-a-scoped-{id}");
        let pool_b = format!("pool-b-scoped-{id}");
        let cap = Capability::Write(Zone::vault("sandbox"));
        grant_for_session(&pool_a, cap.clone());
        assert!(session_grant(&pool_a).contains(&cap));
        // A different pool is unaffected.
        assert!(session_grant(&pool_b).capabilities.is_empty());
    }

    #[test]
    fn clear_empties_all_grants() {
        let id = format!("{:?}", std::thread::current().id());
        let pool = format!("pool-clear-{id}");
        let cap = Capability::Write(Zone::vault("sandbox"));
        grant_for_session(&pool, cap.clone());
        assert!(session_grant(&pool).contains(&cap));

        clear();
        assert!(session_grant(&pool).capabilities.is_empty());
    }
}
