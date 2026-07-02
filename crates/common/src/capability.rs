//! Capability / zone containment model (Decision 4, `liberado-permissions-idea.md`).
//!
//! The security foundation. Inspired by IronClaw's capability-based, zero-ambient-authority
//! design: components start with *nothing* and are granted explicit, named capabilities that
//! can only ever be **narrowed** (attenuated) on delegation, never widened. Enforcement lives
//! at each MCP/hook boundary — this crate provides the vocabulary and the check; the boundary
//! calls it.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A logical scope of data or system access.
///
/// Zones are declared in policy config (Decision 14); the values here are how a capability
/// names the area it applies to. A `Vault` zone corresponds to a top-level vault folder
/// (e.g. `tasks`, `decisions`); a `Named` zone is any non-vault area — an external system or
/// a cross-cutting grouping (well-known examples: `finance`, `sensitive`, `family-shared`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Zone {
    /// A vault folder zone, identified by its top-level path prefix.
    Vault(String),
    /// A named non-vault zone (external system or cross-cutting grouping).
    Named(String),
}

impl Zone {
    /// A vault zone from a folder name, e.g. `Zone::vault("tasks")`.
    pub fn vault(name: impl Into<String>) -> Self {
        Self::Vault(name.into())
    }

    /// A named (non-vault) zone, e.g. `Zone::named("finance")`.
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }
}

/// Per-zone write authority (Decision 5, concurrency spec §3).
///
/// Enforced at the MCP/hook boundary alongside capability checks. The default for an
/// *unlisted* zone is [`WriteClass::ProposalOnly`] (fail safe — agents can never silently
/// write somewhere undeclared).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriteClass {
    /// Agents may read but never write; agent writes here are rejected at the boundary.
    HumanOnly,
    /// Agents may write directly (with provenance + optimistic `expected_hash`).
    AgentWritable,
    /// Agents may not mutate directly; they emit a [`crate::proposal::Proposal`] instead.
    #[default]
    ProposalOnly,
    /// Both humans and agents write; conflicts handled by optimistic concurrency.
    Shared,
}

impl WriteClass {
    /// Whether an agent may write directly to a zone of this class (no proposal required).
    pub fn allows_direct_agent_write(self) -> bool {
        matches!(self, Self::AgentWritable | Self::Shared)
    }
}

/// How reversible and contained an action's effects are — the axis the consequence guard gates on
/// (dispatch spec §6 #3). Ordered by risk (the derived `Ord` follows declaration order), so the
/// guard can compare against a threshold.
///
/// The distinction that matters: a **git-tracked vault write is `Reversible`** (a `git revert` away),
/// while **sending an email or message is `External`** — it leaves the system and can never be taken
/// back. Reversibility, not just "is it a write", is what separates low-risk from high-risk.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Consequence {
    /// No side effects — reads, queries, lookups. Always safe to run.
    #[default]
    ReadOnly,
    /// A change that can be undone *within the system* — e.g. a write/delete in a git-tracked vault,
    /// recoverable from history. Low risk: the safety net is the version control.
    Reversible,
    /// Hard to undo: a write or delete to an **unversioned** store with no recovery path.
    Irreversible,
    /// Leaves the system / is externally visible — sending an email or message, calling an external
    /// API with side effects. The highest risk: irreversible *and* it touches the outside world.
    External,
}

/// How far-reaching an action is — a separate axis from [`Consequence`]. "Delete one note" and
/// "delete *all* notes" have the same (reversible) consequence but very different magnitude; the
/// second is high-stakes even though git makes it recoverable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Magnitude {
    /// A specific, scoped change (one note, one task).
    #[default]
    Bounded,
    /// Affects everything / an unbounded set ("all", "every", a wildcard).
    Sweeping,
}

/// Verbs that destroy or overwrite data. Liberado owns this classification because MCP tools don't
/// declare their own risk — we read the intent from the goal (and from a tool's name/description).
const DESTRUCTIVE_STEMS: &[&str] = &[
    "delet", "remov", "wipe", "purge", "clear", "eras", "destroy", "drop", "truncat", "overwrit",
];

/// Whole-word quantifiers that make an action sweeping.
const SWEEPING_WORDS: &[&str] = &["all", "every", "everything", "entire", "each", "any"];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
}

/// Whether `text` (a goal, or a tool name + description) expresses a destructive operation.
pub fn mentions_destructive(text: &str) -> bool {
    words(text).any(|w| DESTRUCTIVE_STEMS.iter().any(|stem| w.starts_with(stem)))
}

/// Classify how far-reaching `text` is. `Sweeping` when a universal quantifier is present as a whole
/// word (so "install" does not match "all").
pub fn assess_magnitude(text: &str) -> Magnitude {
    if words(text).any(|w| SWEEPING_WORDS.contains(&w.as_str())) {
        Magnitude::Sweeping
    } else {
        Magnitude::Bounded
    }
}

/// The combined high-stakes signal magnitude contributes to the consequence gate: a **sweeping
/// destructive** action ("delete all …") — dangerous by reach even when each change is reversible.
pub fn is_sweeping_destructive(text: &str) -> bool {
    assess_magnitude(text) == Magnitude::Sweeping && mentions_destructive(text)
}

/// An explicit permission within a zone (or to invoke a specific MCP).
///
/// There is no ambient authority: if a `Capability` is not present in the active
/// [`CapabilitySet`], the action is denied.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Read access within a zone.
    Read(Zone),
    /// Write access within a zone.
    Write(Zone),
    /// Read-only access to a summarized/aggregated view (e.g. finance totals without detail).
    ReadSummary(Zone),
    /// Permission to invoke a specific MCP by name.
    ExecuteMcp(String),
}

/// A set of capabilities held by an actor (component, subagent, or dispatch grant).
///
/// The core invariant (Decision 4) is **narrow-only**: [`CapabilitySet::narrow`] can shrink a
/// set on delegation but never widen it. Subagents receive `base ∩ narrowing`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub capabilities: Vec<Capability>,
}

impl CapabilitySet {
    /// An empty set — zero authority (the safe default for any new actor).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Add a capability if not already present.
    pub fn grant(&mut self, cap: Capability) {
        if !self.capabilities.contains(&cap) {
            self.capabilities.push(cap);
        }
    }

    /// Whether this set grants exactly `cap`.
    pub fn contains(&self, cap: &Capability) -> bool {
        self.capabilities.contains(cap)
    }

    /// Whether this set grants permission to invoke the MCP named `mcp`. Avoids allocating a
    /// `Capability::ExecuteMcp(String)` just to test membership — used on the dispatch guard hot
    /// path.
    pub fn grants_mcp(&self, mcp: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| matches!(c, Capability::ExecuteMcp(name) if name == mcp))
    }

    /// The MCP names this set grants `ExecuteMcp` on, in grant order. The runtime-scoping ceiling
    /// derived from a component's capabilities (an empty allow-list elsewhere in the codebase means
    /// "every registered MCP" — this is how callers derive the actual granted set instead).
    pub fn granted_mcps(&self) -> Vec<String> {
        self.capabilities
            .iter()
            .filter_map(|c| match c {
                Capability::ExecuteMcp(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// Narrow this set to the intersection with `other`. This is the **only** way to derive a
    /// delegated set — the result can never contain a capability absent from `self`, so
    /// authority can only shrink down a delegation chain (Decision 4 invariant).
    pub fn narrow(&self, other: &CapabilitySet) -> CapabilitySet {
        CapabilitySet {
            capabilities: self
                .capabilities
                .iter()
                .filter(|c| other.capabilities.contains(c))
                .cloned()
                .collect(),
        }
    }

    /// Guard: return `Ok(())` if `cap` is granted, otherwise [`Error::CapabilityDenied`].
    /// This is the function every MCP/hook boundary calls on entry.
    pub fn check(&self, cap: &Capability) -> Result<()> {
        if self.contains(cap) {
            Ok(())
        } else {
            Err(Error::CapabilityDenied(format!("{cap:?}")))
        }
    }
}

impl FromIterator<Capability> for CapabilitySet {
    /// Collect capabilities into a set, de-duplicating. Enables both
    /// `CapabilitySet::from_iter(..)` and `iter.collect::<CapabilitySet>()`.
    fn from_iter<I: IntoIterator<Item = Capability>>(caps: I) -> Self {
        let mut set = Self::empty();
        for c in caps {
            set.grant(c);
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweeping_destructive_classification() {
        // The case the eval found: reversible (git) but high-magnitude.
        assert!(is_sweeping_destructive("Delete all of my notes."));
        assert!(is_sweeping_destructive("wipe every task"));
        assert!(is_sweeping_destructive("Clear the entire inbox"));

        // Bounded destructive — a single scoped change is not gated by magnitude.
        assert!(!is_sweeping_destructive("delete the note tmp.md"));
        // Sweeping but not destructive — reading everything is fine.
        assert!(!is_sweeping_destructive("summarize all my notes"));
        // Whole-word matching: "install" must not trip the "all" quantifier.
        assert!(!is_sweeping_destructive("install all the packages")); // not destructive anyway
        assert_eq!(assess_magnitude("install the app"), Magnitude::Bounded);
        assert_eq!(assess_magnitude("delete every file"), Magnitude::Sweeping);
    }

    #[test]
    fn narrowing_never_widens() {
        let base = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("tasks")),
            Capability::Write(Zone::vault("tasks")),
            Capability::ExecuteMcp("tasks-mcp".into()),
        ]);

        // A narrowing that *requests* a capability the base lacks cannot introduce it.
        let requested = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("tasks")),
            Capability::Write(Zone::vault("decisions")), // not in base
        ]);

        let narrowed = base.narrow(&requested);
        assert!(narrowed.contains(&Capability::Read(Zone::vault("tasks"))));
        assert!(!narrowed.contains(&Capability::Write(Zone::vault("decisions"))));
        assert!(!narrowed.contains(&Capability::Write(Zone::vault("tasks"))));
    }

    #[test]
    fn check_denies_ungranted() {
        let set = CapabilitySet::from_iter([Capability::Read(Zone::vault("tasks"))]);
        assert!(set.check(&Capability::Read(Zone::vault("tasks"))).is_ok());
        assert!(set.check(&Capability::Write(Zone::vault("tasks"))).is_err());
    }

    #[test]
    fn unlisted_zone_defaults_to_proposal_only() {
        assert_eq!(WriteClass::default(), WriteClass::ProposalOnly);
        assert!(!WriteClass::default().allows_direct_agent_write());
        assert!(WriteClass::AgentWritable.allows_direct_agent_write());
    }
}
