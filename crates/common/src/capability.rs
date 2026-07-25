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
///
/// `clear` is **not** here: the bare adjective in "a clear list" / "clear consolidated summary"
/// was matching via prefix and, combined with sweeping words like `any`, false-positive gated
/// read-only goals (delegate dogfood D1, session `01KX7AGD`). Clear-as-verb is handled in
/// [`mentions_destructive`] with a follower-token check.
const DESTRUCTIVE_STEMS: &[&str] = &[
    "delet", "remov", "wipe", "purge", "eras", "destroy", "drop", "truncat", "overwrit",
];

/// Whole-word forms of "clear" treated as potentially destructive.
const CLEAR_VERB_FORMS: &[&str] = &["clear", "clears", "cleared", "clearing"];

/// Tokens that, immediately after a clear-verb form, make "clear …" read as a destructive verb
/// ("clear the inbox", "clear all notes") rather than an adjective ("a clear list").
const CLEAR_VERB_FOLLOWERS: &[&str] = &[
    "all",
    "every",
    "everything",
    "entire",
    "each",
    "any",
    "the",
    "my",
    "your",
    "our",
    "his",
    "her",
    "their",
    "its",
    "this",
    "that",
    "these",
    "those",
    "out",
    "away",
    "off",
    "up",
];

/// Whole-word quantifiers that make an action sweeping.
///
/// `any` / `each` are intentionally omitted: they appear constantly in benign English
/// ("any details", "each field") and, combined with false-positive destructive stems, over-gated
/// read goals. Strong quantifiers (`all` / `every` / `entire` / `everything`) remain.
const SWEEPING_WORDS: &[&str] = &["all", "every", "everything", "entire"];

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
}

/// Whether `text` (a goal, or a tool name + description) expresses a destructive operation.
pub fn mentions_destructive(text: &str) -> bool {
    let tokens: Vec<String> = words(text).collect();
    if tokens
        .iter()
        .any(|w| DESTRUCTIVE_STEMS.iter().any(|stem| w.starts_with(stem)))
    {
        return true;
    }
    // "clear" only when used as a verb: clear + determiner/quantifier/particle.
    for window in tokens.windows(2) {
        if CLEAR_VERB_FORMS.contains(&window[0].as_str())
            && CLEAR_VERB_FOLLOWERS.contains(&window[1].as_str())
        {
            return true;
        }
    }
    false
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

/// Line-start markers that begin an explanatory section rather than an instruction.
const CONTEXT_MARKERS: &[&str] = &[
    "context:",
    "background:",
    "note:",
    "notes:",
    "fyi:",
    "previously:",
    "history:",
    "for reference:",
];

/// The part of a goal that is an **instruction**, with any trailing explanatory section removed.
///
/// Risk heuristics read a goal as a bag of words, so a sentence *describing* a dangerous action
/// scores identically to one *ordering* it. That is not hypothetical: a delegated goal reading
/// "Mark all 5 tasks complete … Context: earlier, deleting the X task required a permission grant"
/// tripped the sweeping-destructive gate on the word "deleting" — in context the face agent had
/// helpfully added about a *past* action — and the whole request was downgraded to a proposal
/// (homelab, session `01KYD06MT7GFBSHDRVAHC5RYM1`, 2026-07-25).
///
/// Cutting at a line-start marker keeps the guard's real target intact: "Delete all my notes"
/// still gates, with or without a context section after it. What stops gating is *narration*.
///
/// Only a marker at the start of a line counts — "clarify the context: …" mid-sentence is prose,
/// not a section header, and truncating there would silently shrink what the guard inspects.
pub fn instruction_scope(goal: &str) -> &str {
    let mut offset = 0usize;
    for line in goal.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lead = trimmed.to_ascii_lowercase();
        if CONTEXT_MARKERS.iter().any(|m| lead.starts_with(m)) {
            return goal[..offset].trim_end();
        }
        offset += line.len();
    }
    goal.trim_end()
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
    /// Permission to **interrupt a human for guidance** — the human-input channel
    /// (`docs/architecture/channels-and-interactivity.md`, Decision A: interactivity is a
    /// capability, not a session subtype).
    ///
    /// A session whose grant omits this may not await human input: the kernel hands its pack a
    /// closed [`InputChannel`](../../session/src/runner.rs) and rejects any inbound `send_input`.
    /// This is what makes an unattended profile (a cron, a narrow research hat) *structurally*
    /// unable to block on a person who isn't there, rather than merely conventionally so.
    AskHuman,
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

    /// Whether this set permits interrupting a human for guidance ([`Capability::AskHuman`]) — the
    /// check the session kernel makes before wiring up an interactive session's input channel.
    pub fn grants_ask_human(&self) -> bool {
        self.capabilities.contains(&Capability::AskHuman)
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
        assert!(is_sweeping_destructive("clear all tasks"));

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
    fn clear_adjective_and_any_do_not_false_positive_read_goals() {
        // Dogfood D1 (01KX7AGD): face context "Provide a clear consolidated list" + "any tags"
        // must not look like "clear any …" destructive sweeping.
        let dogfood = "Read the Sarah Task List note and any other relationship task notes \
            from TurboVault. Filter to only items with #task. Return a clean list of tasks \
            with their status (pending/complete), content, and any tags.\n\nContext:\n\
            User wants to see relationship tasks. Previous attempt found a file at \
            Tasks/Sarah.md and Life/Relationships/ notes, but results were cut off. \
            Provide a clear consolidated list.";
        assert!(!mentions_destructive(dogfood));
        assert!(!is_sweeping_destructive(dogfood));
        assert!(!is_sweeping_destructive(
            "Return a clear list of all pending tasks with any tags"
        ));
        // Verb usage still counts.
        assert!(mentions_destructive("clear the inbox"));
        assert!(mentions_destructive("clear all notes"));
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

#[cfg(test)]
mod instruction_scope_tests {
    use super::*;

    /// The verbatim goal from homelab session `01KYD06MT7GFBSHDRVAHC5RYM1`, which was downgraded
    /// to a proposal because the word "deleting" appears in the face agent's trailing context.
    const REAL_GOAL: &str = "Mark all 5 of these RTX onboarding tasks as complete (set done_date to 2026-07-24):\n\n1. Understand MPD team workflow end-to-end\n2. Complete security & compliance training\n\nThese are all in Work/RTX/Onboarding.md.\n\nContext:\nEarlier, deleting the \"Update address\" task from Work/RTX/Onboarding.md required a permission grant for the Work zone (perm-1784692954895166502). That proposal may or may not have been approved. If write access to Work is still blocked, file the necessary permission request.";

    #[test]
    fn the_real_false_positive_no_longer_gates() {
        assert!(
            is_sweeping_destructive(REAL_GOAL),
            "precondition: the unscoped goal really did trip the gate"
        );
        assert!(
            !is_sweeping_destructive(instruction_scope(REAL_GOAL)),
            "marking tasks complete must not read as a sweeping deletion just because the \
             context section mentions a past one"
        );
    }

    /// The guard must keep its teeth. Narration is what stops gating, not destruction.
    #[test]
    fn a_real_sweeping_deletion_still_gates_with_or_without_context() {
        for goal in [
            "Delete all my notes",
            "Delete all my notes\n\nContext: I already backed them up.",
            "Remove every file in the archive\n\nNote: discussed yesterday.",
        ] {
            assert!(
                is_sweeping_destructive(instruction_scope(goal)),
                "must still gate: {goal:?}"
            );
        }
    }

    #[test]
    fn only_a_line_start_marker_truncates() {
        // Mid-sentence "context:" is prose. Truncating there would silently shrink what the guard
        // inspects — the opposite failure, and a much worse one.
        let goal = "Delete all notes to clarify the context: they are stale";
        assert_eq!(instruction_scope(goal), goal);
        assert!(is_sweeping_destructive(instruction_scope(goal)));
    }

    #[test]
    fn markers_are_recognized_case_insensitively_and_indented() {
        for marker in [
            "Context:",
            "context:",
            "  NOTE:",
            "Background:",
            "FYI:",
            "Previously:",
        ] {
            let goal = format!("Mark all tasks done\n\n{marker} we deleted one earlier");
            assert_eq!(instruction_scope(&goal), "Mark all tasks done");
            assert!(
                !is_sweeping_destructive(instruction_scope(&goal)),
                "{marker}"
            );
        }
    }

    #[test]
    fn a_goal_with_no_marker_is_returned_whole() {
        let goal = "Mark all 5 tasks complete";
        assert_eq!(instruction_scope(goal), goal);
        assert_eq!(instruction_scope("  "), "");
    }

    #[test]
    fn a_goal_that_is_only_context_scopes_to_empty_and_cannot_gate() {
        let goal = "Context: we deleted all the notes";
        assert_eq!(instruction_scope(goal), "");
        assert!(!is_sweeping_destructive(instruction_scope(goal)));
    }
}
