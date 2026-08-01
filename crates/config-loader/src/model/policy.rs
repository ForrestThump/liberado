//! Policy section: zones and grants.

use liberado_common::{Capability, CapabilitySet, WriteClass};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Policy — the central, auditable security surface (Decision 4 / 5).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Per-zone write classes. An *unlisted* zone is treated as `proposal_only` (fail safe).
    pub zones: Vec<ZonePolicy>,
    /// Base capability grants per component (narrowed, never widened, at dispatch).
    pub grants: Vec<Grant>,
    /// Names of secrets referenced by components (resolved from env/systemd, never inlined).
    pub secret_refs: Vec<String>,
}

impl Policy {
    /// The write class declared for `zone`, or the fail-safe default if unlisted.
    pub fn write_class(&self, zone: &str) -> WriteClass {
        self.zones
            .iter()
            .find(|z| z.zone == zone)
            .map(|z| z.write_class)
            .unwrap_or_default()
    }

    /// The capability set granted to `component` — the union of every [`Grant`] whose `component`
    /// matches, narrowed to just that slice of authority (this narrowing is itself the ceiling; a
    /// dispatch further narrows within it, never outside it — Decision 4). Two components are
    /// meaningful today: `"main-agent"` (the chat-facing tool surface, `ChatSessions`) and
    /// `"dispatcher"` (the ceiling the guard pipeline and `ExecuteDirect`/`DispatchSubagent`
    /// execution check against, `configure_daemon`). A grant can list either, both, or neither —
    /// an MCP granted only to `"dispatcher"` is reachable via dispatch-routed execution but never
    /// appears directly in chat.
    pub fn capabilities_for(&self, component: &str) -> CapabilitySet {
        self.grants
            .iter()
            .filter(|g| g.component == component)
            .flat_map(|g| g.capabilities.iter().cloned())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonePolicy {
    /// Zone name (a vault folder, or a named external zone).
    pub zone: String,
    pub write_class: WriteClass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    /// Component the grant applies to (MCP/hook/subagent role name).
    pub component: String,
    pub capabilities: Vec<Capability>,
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_write_class() -> impl Strategy<Value = WriteClass> {
        prop_oneof![
            Just(WriteClass::AgentWritable),
            Just(WriteClass::HumanOnly),
            Just(WriteClass::ProposalOnly),
        ]
    }

    proptest! {
        #[test]
        fn proptest_write_class_first_match_wins(
            zones in proptest::collection::vec(
                ("[a-zA-Z0-9]{1,20}", arb_write_class()),
                1..10,
            ),
            query_zone in "[a-zA-Z0-9]{1,20}",
        ) {
            let policy = Policy {
                zones: zones.iter().map(|(name, class)| ZonePolicy {
                    zone: name.clone(),
                    write_class: *class,
                }).collect(),
                ..Policy::default()
            };
            let expected = zones.iter()
                .find(|(name, _)| name == &query_zone)
                .map(|(_, class)| *class)
                .unwrap_or_default();
            prop_assert_eq!(policy.write_class(&query_zone), expected);
        }

        #[test]
        fn proptest_capabilities_for_union(
            grants in proptest::collection::vec(
                ("[a-z]{1,10}", proptest::collection::vec("[a-z]{1,10}", 1..5)),
                1..5,
            ),
            component in "[a-z]{1,10}",
        ) {
            let all_grants: Vec<Grant> = grants.iter().map(|(comp, mcp_names)| Grant {
                component: comp.clone(),
                capabilities: mcp_names.iter()
                    .map(|n| Capability::ExecuteMcp(n.clone()))
                    .collect(),
            }).collect();
            let policy = Policy {
                grants: all_grants.clone(),
                ..Policy::default()
            };
            let caps = policy.capabilities_for(&component);
            let expected: Vec<Capability> = all_grants.iter()
                .filter(|g| g.component == component)
                .flat_map(|g| g.capabilities.iter().cloned())
                .collect();
            prop_assert_eq!(caps.capabilities.len(), expected.len());
            for c in &expected {
                prop_assert!(caps.capabilities.contains(c));
            }
        }

        #[test]
        fn proptest_unlisted_zone_is_proposal_only(
            zones in proptest::collection::vec(
                ("[a-zA-Z0-9]{1,20}", arb_write_class()),
                1..10,
            ),
        ) {
            let policy = Policy {
                zones: zones.iter().map(|(name, class)| ZonePolicy {
                    zone: name.clone(),
                    write_class: *class,
                }).collect(),
                ..Policy::default()
            };
            // A zone not in the list must return ProposalOnly (fail-safe default)
            prop_assert_eq!(
                policy.write_class("zzz-definitely-not-in-any-list"),
                WriteClass::ProposalOnly
            );
        }
    }
}
