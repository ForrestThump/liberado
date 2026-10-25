//! Whether a grant may hand work to another agent.
//!
//! Delegation is the one authority question a grant answers about *other* agents rather than
//! about itself, so it lives beside [`CapabilitySet`] rather than inside it: the shape of the
//! set says nothing about delegation until these two rules read it.

use super::{Capability, CapabilitySet};

impl CapabilitySet {
    /// Whether every capability is a read, and there is at least one.
    ///
    /// `Read` and `ReadSummary` only. An empty set is not read-only: a profile that
    /// says nothing about authority still inherits the daemon's delegation default.
    /// A write, a tool, or `AskHuman` is not a read. A coding chat that can ask a
    /// person still delegates.
    pub fn authorizes_only_reads(&self) -> bool {
        !self.capabilities.is_empty()
            && self
                .capabilities
                .iter()
                .all(|cap| matches!(cap, Capability::Read(_) | Capability::ReadSummary(_)))
    }

    /// Whether this grant may `delegate`.
    ///
    /// An explicit `Some` wins. A read-only grant with no setting does not delegate.
    /// Any other unset grant inherits `daemon_default`.
    pub fn delegation_allowed(&self, explicit: Option<bool>, daemon_default: bool) -> bool {
        match explicit {
            Some(value) => value,
            None if self.authorizes_only_reads() => false,
            None => daemon_default,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::capability::{Capability, CapabilitySet, Zone};

    #[test]
    fn authorizes_only_reads_is_reads_and_nothing_else() {
        let reads = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("tasks")),
            Capability::ReadSummary(Zone::named("finance")),
        ]);
        assert!(reads.authorizes_only_reads());
        assert!(!CapabilitySet::empty().authorizes_only_reads());

        let with_write = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("tasks")),
            Capability::Write(Zone::vault("tasks")),
        ]);
        assert!(!with_write.authorizes_only_reads());

        let with_tool = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("tasks")),
            Capability::ExecuteMcp("tasks-mcp".into()),
        ]);
        assert!(!with_tool.authorizes_only_reads());

        let with_ask = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("decisions")),
            Capability::AskHuman,
        ]);
        assert!(!with_ask.authorizes_only_reads());
    }

    #[test]
    fn delegation_allowed_turns_read_only_off_unless_explicit() {
        let reads = CapabilitySet::from_iter([Capability::Read(Zone::vault("tasks"))]);
        assert!(!reads.delegation_allowed(None, true));
        assert!(reads.delegation_allowed(Some(true), false));
        assert!(!reads.delegation_allowed(Some(false), true));

        let empty = CapabilitySet::empty();
        assert!(empty.delegation_allowed(None, true));
        assert!(!empty.delegation_allowed(None, false));

        let writer = CapabilitySet::from_iter([Capability::Write(Zone::vault("tasks"))]);
        assert!(writer.delegation_allowed(None, true));
    }
}
