//! Capability / zone containment model (Decision 4, `liberado-permissions-idea.md`).
//!
//! The security foundation. Inspired by IronClaw's capability-based, zero-ambient-authority
//! design: components start with *nothing* and are granted explicit, named capabilities that
//! can only ever be **narrowed** (attenuated) on delegation, never widened. Enforcement lives
//! at each MCP/hook boundary — this crate provides the vocabulary and the check; the boundary
//! calls it.

use serde::{Deserialize, Serialize};

use crate::dispatch::mcp_of;
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

/// The consequence at or above which an action needs a human in the loop.
///
/// `Irreversible`, so anything that cannot be undone — and everything `External` — is gated, while
/// `Reversible` (git-tracked) writes and `ReadOnly` lookups flow.
///
/// Lives here rather than in the dispatcher because it is now read by two independent guards that
/// must agree: the dispatcher's consequence guard (which downgrades a risky action to a `Clarify`
/// or `Propose`), and the orchestrator's delivery guard (which decides whether a report may bypass
/// the main agent). Two copies of this threshold drifting apart would mean an action the dispatcher
/// considers safe enough to run without asking is simultaneously considered too dangerous to report
/// directly, or worse, the reverse.
pub const CONSEQUENCE_GATE: Consequence = Consequence::Irreversible;

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
            return truncate_to_instruction(&goal[..offset]);
        }
        offset += line.len();
    }
    truncate_to_instruction(goal)
}

/// How much of a goal the risk heuristics will read as an *instruction*.
///
/// The heuristics are a bag of words, and their target is a short imperative — "delete all my
/// notes". Past this length a goal is not an instruction, it is an instruction **plus content**,
/// and every long document contains "all" and "remove" somewhere.
///
/// That is not a hypothetical either: a live goal of 10,364 characters — a research report the face
/// agent pasted inline so a subagent could file it — scored `Sweeping` on 5 quantifier hits and
/// `destructive` on a stem buried in the prose, and the whole write was downgraded to a proposal the
/// human had to approve (homelab, `chat-delegate-01KYE9FYWWNANRR864P3SXF07C`, 2026-07-26).
/// `CONTEXT_MARKERS` did not help: pasted content carries no `Context:` line, so nothing was cut.
///
/// Capping is safe because this is a **pre-flight** heuristic, not the boundary. Every actual call
/// still passes `RiskGatedToolRuntime`, which assesses the concrete arguments of the concrete tool —
/// a real "delete all" arrives there as arguments, where it is caught regardless of what any goal
/// text said.
const INSTRUCTION_SCAN_LIMIT: usize = 600;

/// Trim to [`INSTRUCTION_SCAN_LIMIT`] on a character boundary, then drop trailing whitespace.
fn truncate_to_instruction(s: &str) -> &str {
    let s = s.trim_end();
    if s.len() <= INSTRUCTION_SCAN_LIMIT {
        return s;
    }
    let mut end = INSTRUCTION_SCAN_LIMIT;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end()
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
    /// Permission to invoke a specific MCP by name — **every** tool it exposes.
    ExecuteMcp(String),
    /// Permission to invoke one named tool, as `"<mcp>:<tool>"` — e.g.
    /// `ExecuteTool("turbovault:read_note")`.
    ///
    /// The finer half of [`ExecuteMcp`], for a profile that wants a handful of an MCP's tools rather
    /// than the whole server. `ExecuteMcp(m)` **subsumes** every `ExecuteTool("m:…")`, which is what
    /// keeps the two from being separate authority systems: see
    /// [`Capability::subsumes`] and [`CapabilitySet::narrow`].
    ///
    /// Granting the *server* is still the right default for a trusted pack; this exists because
    /// "read my notes but do not write them" cannot be said any other way, and a coarse grant plus a
    /// hopeful prompt is not a boundary.
    ExecuteTool(String),
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

impl Capability {
    /// Whether holding `self` already implies holding `other`.
    ///
    /// Only one relation is non-trivial: `ExecuteMcp(m)` implies `ExecuteTool("m:anything")`, because
    /// granting a server grants its tools. Everything else is plain equality — zones deliberately do
    /// **not** nest (`Read(Work)` does not imply `Read(Work/Sub)`; zones are flat names).
    ///
    /// This exists because [`CapabilitySet::narrow`] used naive equality, and adding
    /// [`ExecuteTool`](Capability::ExecuteTool) would have made
    /// `ExecuteMcp("turbovault") ∩ ExecuteTool("turbovault:read_note")` come out **empty** — silently
    /// revoking a delegated subagent's tools rather than failing loudly. A partial order is the only
    /// thing that makes coarse and fine grants interoperate.
    pub fn subsumes(&self, other: &Capability) -> bool {
        if self == other {
            return true;
        }
        match (self, other) {
            (Capability::ExecuteMcp(mcp), Capability::ExecuteTool(qualified)) => {
                mcp_of(qualified) == mcp
            }
            _ => false,
        }
    }

    /// The MCP this capability concerns, if any — the server for [`ExecuteMcp`](Capability::ExecuteMcp),
    /// the owning server for [`ExecuteTool`](Capability::ExecuteTool).
    pub fn mcp_name(&self) -> Option<&str> {
        match self {
            Capability::ExecuteMcp(mcp) => Some(mcp),
            Capability::ExecuteTool(qualified) => Some(mcp_of(qualified)),
            _ => None,
        }
    }
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

    /// Whether this set permits touching the MCP named `mcp` **at all** — the whole server, or any
    /// single tool on it.
    ///
    /// A coarse question, and the coarseness is deliberate: it answers "should this MCP be visible /
    /// connected / listed", not "may this call proceed". Use [`grants_tool`](Self::grants_tool) for
    /// the latter. Before `ExecuteTool` existed the two questions were the same one, so every caller
    /// asking this got the precise answer for free — check any security-relevant caller you find
    /// still using it.
    ///
    /// Avoids allocating a `Capability::ExecuteMcp(String)` just to test membership — used on the
    /// dispatch guard hot path.
    pub fn grants_mcp(&self, mcp: &str) -> bool {
        self.capabilities.iter().any(|c| c.mcp_name() == Some(mcp))
    }

    /// Whether this set permits invoking the specific tool `qualified` (`"<mcp>:<tool>"`).
    ///
    /// **This is the authorization question.** True when the set grants the owning server outright, or
    /// grants exactly this tool. A bare name with no `:` is treated as its own MCP, matching
    /// [`mcp_of`]'s fallback, so an unprefixed tool is not accidentally authorized by an unrelated
    /// grant.
    pub fn grants_tool(&self, qualified: &str) -> bool {
        let mcp = mcp_of(qualified);
        self.capabilities.iter().any(|c| match c {
            Capability::ExecuteMcp(name) => name == mcp,
            Capability::ExecuteTool(name) => name == qualified,
            _ => false,
        })
    }

    /// The individually granted tools, as `"<mcp>:<tool>"`, in grant order.
    ///
    /// Only [`ExecuteTool`](Capability::ExecuteTool) entries — a server-wide `ExecuteMcp` is *not*
    /// expanded here, because this set has no way to enumerate a server's tools. Callers deciding
    /// what to show a model need both this and [`granted_mcps`](Self::granted_mcps).
    pub fn granted_tools(&self) -> Vec<String> {
        self.matching_names(|c| matches!(c, Capability::ExecuteTool(_)))
    }

    /// Whether this set permits interrupting a human for guidance ([`Capability::AskHuman`]) — the
    /// check the session kernel makes before wiring up an interactive session's input channel.
    pub fn grants_ask_human(&self) -> bool {
        self.capabilities.contains(&Capability::AskHuman)
    }

    /// Shared helper for [`granted_tools`](Self::granted_tools) and
    /// [`granted_mcps`](Self::granted_mcps): collect the inner string from every variant that
    /// matches `predicate`. Panics on non-Execute variants (the predicate enforces correctness).
    fn matching_names(&self, predicate: impl Fn(&Capability) -> bool) -> Vec<String> {
        self.capabilities
            .iter()
            .filter(|c| predicate(c))
            .map(|c| match c {
                Capability::ExecuteTool(name) | Capability::ExecuteMcp(name) => name.clone(),
                _ => unreachable!("matching_names predicate should only match Execute variants"),
            })
            .collect()
    }

    /// The MCP names this set grants `ExecuteMcp` on, in grant order. The runtime-scoping ceiling
    /// derived from a component's capabilities (an empty allow-list elsewhere in the codebase means
    /// "every registered MCP" — this is how callers derive the actual granted set instead).
    pub fn granted_mcps(&self) -> Vec<String> {
        self.matching_names(|c| matches!(c, Capability::ExecuteMcp(_)))
    }

    /// Narrow this set to the intersection with `other`. This is the **only** way to derive a
    /// delegated set — the result can never contain a capability absent from `self`, so
    /// authority can only shrink down a delegation chain (Decision 4 invariant).
    /// Narrow this set to what `other` also permits.
    ///
    /// # Not a set intersection
    ///
    /// Membership is decided by [`Capability::subsumes`], so a coarse grant on one side authorizes a
    /// fine request on the other. Two cases, and the result is the **narrower** of the pair each time:
    ///
    /// * `self` has `ExecuteMcp("tv")`, `other` asks for `ExecuteTool("tv:read")` → keep
    ///   `ExecuteTool("tv:read")`. The narrowing asked for less; honor it.
    /// * `self` has `ExecuteTool("tv:read")`, `other` allows `ExecuteMcp("tv")` → keep
    ///   `ExecuteTool("tv:read")`. We never held more than the one tool; a permissive narrowing
    ///   cannot invent the rest.
    ///
    /// Plain equality — what this used to do — got the first case **wrong in the dangerous
    /// direction**: no entry matched, so the result was empty and a delegated subagent silently lost
    /// every tool. Never a security hole (empty is the safe end) but an invisible functional one,
    /// which is worse to debug.
    ///
    /// The invariant still holds: every returned capability is subsumed by something in `self`, so
    /// authority can only shrink down a delegation chain (Decision 4).
    pub fn narrow(&self, other: &CapabilitySet) -> CapabilitySet {
        let mut narrowed = CapabilitySet::empty();
        for mine in &self.capabilities {
            for theirs in &other.capabilities {
                // Whichever side is more specific is the one that survives; when they are equal
                // either branch yields the same value.
                if mine.subsumes(theirs) {
                    narrowed.grant(theirs.clone());
                } else if theirs.subsumes(mine) {
                    narrowed.grant(mine.clone());
                }
            }
        }
        narrowed
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

    /// The live false positive that motivated the cap: a 10,364-char report the face agent pasted
    /// inline as a goal so a subagent could file it. Five quantifier hits and a destructive stem
    /// buried in prose downgraded a plain vault write to a proposal the human had to approve
    /// (homelab `chat-delegate-01KYE9FYWWNANRR864P3SXF07C`, 2026-07-26). `CONTEXT_MARKERS` could
    /// not help — pasted content carries no `Context:` line.
    ///
    /// Shaped like the real goal: a one-line instruction, then a document whose incidental
    /// "all"/"remove" language sits well past the instruction. In the live case the first such word
    /// appeared beyond 2,000 characters, so the 600-char limit clears it by more than 3x.
    #[test]
    fn a_pasted_document_is_not_read_as_a_sweeping_instruction() {
        let mut goal = String::from(
            "Write a comprehensive markdown report to the vault at path: Learning/report.md

             Use this synthesized content based on research already conducted.

# Findings

             The ecosystem matured considerably over the period under review, with several              independent implementations reaching production readiness.

",
        );
        // Body text, as a real report has, before any incidental risk-shaped language appears.
        for i in 0..12 {
            goal.push_str(&format!(
                "Section {i}: implementations converged on a shared interface definition,                  and adoption followed once the tooling stabilised.

"
            ));
        }
        // Incidental destructive/quantifier language, as any long technical document contains.
        goal.push_str(
            "## Operations

The runtime removes all stale entries during compaction, and every              node purges its entire cache on restart.
",
        );
        assert!(goal.len() > INSTRUCTION_SCAN_LIMIT * 2);
        assert!(
            !is_sweeping_destructive(instruction_scope(&goal)),
            "content past the instruction must not gate the write"
        );
    }

    /// The cap's honest limit, recorded rather than hidden: a document whose *opening* reads
    /// destructive still trips the guard. That is acceptable — the failure mode is a proposal (an
    /// approval tap), not an unsafe write, and the real boundary is `RiskGatedToolRuntime`, which
    /// reads the concrete arguments of the concrete call.
    #[test]
    fn a_document_opening_with_destructive_prose_is_a_known_false_positive() {
        let goal = format!(
            "Save this note.

We deleted all the old records last quarter.{}",
            "x".repeat(INSTRUCTION_SCAN_LIMIT)
        );
        assert!(is_sweeping_destructive(instruction_scope(&goal)));
    }

    #[test]
    fn a_short_destructive_instruction_still_trips_the_guard() {
        assert!(is_sweeping_destructive(instruction_scope(
            "Delete all my notes in the Journal folder"
        )));
        // Still caught when a Context: section follows.
        assert!(is_sweeping_destructive(instruction_scope(
            "Remove every task from the inbox

Context: the user asked twice."
        )));
    }

    #[test]
    fn instruction_scope_truncates_on_a_character_boundary() {
        let goal = "🎉".repeat(1000);
        let scoped = instruction_scope(&goal);
        assert!(scoped.len() <= INSTRUCTION_SCAN_LIMIT);
        assert!(goal.starts_with(scoped), "must remain a valid prefix");
    }
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

    // ── Per-tool grants (`ExecuteTool`) ─────────────────────────────────────────────────────
    //
    // The pair `ExecuteMcp` / `ExecuteTool` is a partial order, not two flat variants, and `narrow`
    // is where getting that wrong does damage. These pin both directions of the relation.

    #[test]
    fn execute_mcp_subsumes_its_tools_but_not_the_reverse() {
        let server = Capability::ExecuteMcp("turbovault".into());
        let one_tool = Capability::ExecuteTool("turbovault:read_note".into());
        let other_server = Capability::ExecuteTool("tasks-mcp:add".into());

        assert!(server.subsumes(&one_tool));
        assert!(
            !one_tool.subsumes(&server),
            "one tool is not the whole server"
        );
        assert!(
            !server.subsumes(&other_server),
            "a grant must not leak across MCPs"
        );
        assert!(one_tool.subsumes(&one_tool), "subsumption is reflexive");
    }

    #[test]
    fn grants_tool_is_the_authorization_question() {
        let whole = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
        assert!(whole.grants_tool("turbovault:read_note"));
        assert!(whole.grants_tool("turbovault:write_note"));

        let partial =
            CapabilitySet::from_iter([Capability::ExecuteTool("turbovault:read_note".into())]);
        assert!(partial.grants_tool("turbovault:read_note"));
        assert!(
            !partial.grants_tool("turbovault:write_note"),
            "the point of a per-tool grant: the rest of the server stays shut"
        );

        // `grants_mcp` is the coarse question and answers yes for a partial grant — that is correct
        // (the MCP is reachable) and is exactly why it must not be used to authorize a call.
        assert!(partial.grants_mcp("turbovault"));
        assert!(!partial.grants_mcp("tasks-mcp"));
    }

    /// The regression this whole change turns on. Under the old `narrow` — a plain set intersection —
    /// this returned **empty**, silently stripping a delegated subagent of tools its parent held.
    #[test]
    fn narrowing_a_server_grant_to_one_tool_keeps_that_tool() {
        let base = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
        let requested =
            CapabilitySet::from_iter([Capability::ExecuteTool("turbovault:read_note".into())]);

        let narrowed = base.narrow(&requested);

        assert!(narrowed.grants_tool("turbovault:read_note"));
        assert!(
            !narrowed.grants_tool("turbovault:write_note"),
            "narrowing asked for one tool, so only that tool survives"
        );
    }

    /// The other direction: a permissive narrowing cannot re-inflate a grant we never held.
    #[test]
    fn a_permissive_narrowing_cannot_widen_a_tool_grant_to_the_server() {
        let base =
            CapabilitySet::from_iter([Capability::ExecuteTool("turbovault:read_note".into())]);
        let requested = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);

        let narrowed = base.narrow(&requested);

        assert!(narrowed.grants_tool("turbovault:read_note"));
        assert!(
            !narrowed.grants_tool("turbovault:write_note"),
            "Decision 4: delegation may not widen, whatever the narrowing asks for"
        );
        assert!(!narrowed.contains(&Capability::ExecuteMcp("turbovault".into())));
    }

    #[test]
    fn tool_grants_do_not_leak_between_mcps() {
        let base =
            CapabilitySet::from_iter([Capability::ExecuteTool("turbovault:read_note".into())]);
        let requested =
            CapabilitySet::from_iter([Capability::ExecuteTool("tasks-mcp:read_note".into())]);
        assert_eq!(base.narrow(&requested), CapabilitySet::empty());
    }

    #[test]
    fn granted_tools_lists_only_the_individual_grants() {
        let set = CapabilitySet::from_iter([
            Capability::ExecuteMcp("spider-mcp".into()),
            Capability::ExecuteTool("turbovault:read_note".into()),
            Capability::ExecuteTool("turbovault:search_notes".into()),
        ]);
        assert_eq!(
            set.granted_tools(),
            vec!["turbovault:read_note", "turbovault:search_notes"]
        );
        // A server-wide grant is not expanded — this set cannot know what tools spider-mcp has.
        assert_eq!(set.granted_mcps(), vec!["spider-mcp"]);
    }

    /// A bare name with no `:` must not be authorized by an unrelated grant. `mcp_of` treats it as
    /// its own MCP, and `grants_tool` has to agree, or an unprefixed tool name becomes a wildcard.
    #[test]
    fn an_unprefixed_tool_name_is_not_authorized_by_another_grant() {
        let set = CapabilitySet::from_iter([Capability::ExecuteMcp("turbovault".into())]);
        assert!(!set.grants_tool("read_note"));
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

    #[test]
    fn grants_ask_human_is_false_without_ask_human() {
        let set = CapabilitySet::from_iter([
            Capability::Read(Zone::vault("tasks")),
            Capability::ExecuteMcp("turbovault".into()),
        ]);
        assert!(!set.grants_ask_human());

        let with_ask = CapabilitySet::from_iter([
            Capability::AskHuman,
            Capability::Read(Zone::vault("tasks")),
        ]);
        assert!(with_ask.grants_ask_human());
    }

    #[test]
    fn mcp_name_returns_some_for_execute_mcp_and_execute_tool() {
        let mcp = Capability::ExecuteMcp("turbovault".into());
        assert_eq!(mcp.mcp_name(), Some("turbovault"));

        let tool = Capability::ExecuteTool("turbovault:read_note".into());
        assert_eq!(tool.mcp_name(), Some("turbovault"));
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

    /// A goal at exactly INSTRUCTION_SCAN_LIMIT bytes should not be truncated.
    /// The `>` on line 273 controls this: `end > 0` exits the snap loop when `end` is already 0.
    #[test]
    fn truncate_at_exact_limit_preserves_full_length() {
        let ascii = "a".repeat(INSTRUCTION_SCAN_LIMIT);
        assert_eq!(instruction_scope(&ascii).len(), INSTRUCTION_SCAN_LIMIT);

        // One byte past the limit — must snap to a boundary (all ASCII here).
        let over = format!("{}b", ascii);
        assert!(instruction_scope(&over).len() <= INSTRUCTION_SCAN_LIMIT);
    }

    /// An instruction with a Context: marker whose instruction part is exactly at the limit.
    #[test]
    fn context_marker_truncates_before_instruction_limit() {
        let goal = "a goal under the limit\n\nContext: we deleted all the notes";
        assert!(instruction_scope(goal).len() < INSTRUCTION_SCAN_LIMIT);
    }

    /// When the instruction scan limit lands mid-character, the snap loop must move `end` back to
    /// a valid boundary. Without the snap (`>` → `==`/`<`), slicing at `INSTRUCTION_SCAN_LIMIT`
    /// would panic; with the wrong direction (`-=` → `+=`) the window would overshoot.
    #[test]
    fn truncate_snaps_to_char_boundary_when_cutoff_is_mid_char() {
        let s = format!("{}🎉", "a".repeat(599));
        assert!(s.len() > INSTRUCTION_SCAN_LIMIT);
        assert!(
            !s.is_char_boundary(INSTRUCTION_SCAN_LIMIT),
            "precondition: cutoff is mid-char"
        );
        let scoped = instruction_scope(&s);
        assert!(
            scoped.len() < INSTRUCTION_SCAN_LIMIT,
            "scope must snap back from cutoff: {}",
            scoped.len()
        );
        assert!(
            scoped.len() >= INSTRUCTION_SCAN_LIMIT - 4,
            "snap should only drop the partial char: {}",
            scoped.len()
        );
    }
}

/// Property tests for the goal-reading heuristics: scope must be a valid prefix, must be invariant
/// under an appended `Context:` section, and must never panic on arbitrary (or pathological)
/// Unicode. Plus the destructive classifiers' case-invariance and stem-detection guarantees.
#[cfg(test)]
mod goal_heuristics_proptest_tests {
    use super::*;
    use proptest::prelude::*;

    /// Arbitrary Unicode text, 0-200 chars, unioned with rare pathological seeds: empty,
    /// whitespace-only, emoji-heavy (exercises char-boundary truncation), Zalgo combining-mark
    /// text, and a multi-megabyte string well past the 600-byte `INSTRUCTION_SCAN_LIMIT`.
    fn text_strategy() -> impl Strategy<Value = String> {
        let arbitrary = proptest::collection::vec(proptest::char::any(), 0..=200)
            .prop_map(|chars| chars.into_iter().collect::<String>());
        prop_oneof![
            10 => arbitrary,
            1 => Just(String::new()),
            1 => Just(" \t\r\n ".to_string()),
            1 => Just("🎉".repeat(200)),
            1 => Just(
                "z\u{338}a\u{301}\u{300}l\u{338}g\u{323}\u{327}o\u{301} \
                 t\u{338}e\u{301}x\u{338}t\u{301}"
                    .to_string()
            ),
            1 => Just("x".repeat(2_000_000)),
        ]
    }

    /// Short arbitrary Unicode stems for the known-stem property (it builds nine candidate words
    /// per case; the multi-megabyte seed belongs only in `text_strategy`).
    fn stem_strategy() -> impl Strategy<Value = String> {
        proptest::collection::vec(proptest::char::any(), 0..=64)
            .prop_map(|chars| chars.into_iter().collect::<String>())
    }

    /// The scope the heuristics inspect is always a prefix of the goal.
    fn scope_is_valid_prefix(goal: String) -> bool {
        let scope = instruction_scope(&goal);
        goal.starts_with(scope)
    }

    /// None of the goal-reading functions may panic on arbitrary input.
    fn scope_never_panics(goal: String) -> bool {
        let _ = instruction_scope(&goal);
        let _ = is_sweeping_destructive(&goal);
        let _ = mentions_destructive(&goal);
        true
    }

    /// Appending an explanatory `Context:` section never changes what the heuristics read.
    fn scope_context_invariant(goal: String) -> bool {
        let scope = instruction_scope(&goal);
        let with_ctx = format!("{}\n\nContext: additional notes", goal);
        instruction_scope(&with_ctx) == scope
    }

    /// The destructive classifiers do not depend on input casing.
    fn case_invariant(text: String) -> bool {
        is_sweeping_destructive(&text.to_lowercase()) == is_sweeping_destructive(&text)
            && mentions_destructive(&text.to_lowercase()) == mentions_destructive(&text)
    }

    /// Every stem in `DESTRUCTIVE_STEMS`, followed by an arbitrary suffix, must be detected.
    fn known_stems_detected(stem: String) -> bool {
        let stems = [
            "delet", "remov", "wipe", "purge", "eras", "destroy", "drop", "truncat", "overwrit",
        ];
        for s in &stems {
            let word = format!("{}{}", s, &stem);
            if word.contains(s) && !mentions_destructive(&word) {
                return false;
            }
        }
        true
    }

    proptest! {
        #[test]
        fn scope_is_a_valid_prefix(goal in text_strategy()) {
            prop_assert!(scope_is_valid_prefix(goal));
        }

        #[test]
        fn scope_and_classifiers_never_panic(goal in text_strategy()) {
            prop_assert!(scope_never_panics(goal));
        }

        #[test]
        fn scope_is_context_invariant(goal in text_strategy()) {
            prop_assert!(scope_context_invariant(goal));
        }

        #[test]
        fn destructive_classification_is_case_invariant(text in text_strategy()) {
            prop_assert!(case_invariant(text));
        }

        #[test]
        fn known_destructive_stems_are_detected(stem in stem_strategy()) {
            prop_assert!(known_stems_detected(stem));
        }
    }
}

/// Property-based checks of the `narrow`-only invariants (Decision 4) and the algebraic shape of
/// [`CapabilitySet::narrow`]. The input strategy is deliberately independent (no correlation between
/// an `ExecuteMcp` name and an `ExecuteTool` prefix), so the non-trivial `ExecuteMcp ⊃ ExecuteTool`
/// subsumption arm is hit only on exact string coincidence — the equality arms of `subsumes` carry
/// these checks in practice. A correlated strategy (tool names derived from a generated MCP name)
/// would exercise the partial order directly, at the cost of hitting an *ordering* artifact of the
/// derived `PartialEq` (a `Vec` compare) that is not a semantic narrowing failure.
///
/// Note on variants: the requested list (`Read`, `Write(Zone)`, `ExecuteMcp`, `ExecuteTool`,
/// `AskHuman`, `Delegate`, `PerformTask`) predates this codebase — the enum here has no `Delegate`
/// or `PerformTask`. The generator covers the six variants that actually exist, including
/// `ReadSummary(Zone)`.
#[cfg(test)]
mod proptest_tests {
    use proptest::prelude::*;

    use super::*;

    // ── Arbitrary impls ────────────────────────────────────────────────────────────────────────

    impl Arbitrary for Capability {
        type Parameters = ();
        type Strategy = BoxedStrategy<Capability>;

        fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
            // A zone name: 1-20 alphanumeric or `/` chars, either flavor of zone.
            let zone = prop_oneof![
                "[a-zA-Z0-9/]{1,20}".prop_map(Zone::Vault),
                "[a-zA-Z0-9/]{1,20}".prop_map(Zone::Named),
            ];
            prop_oneof![
                zone.clone().prop_map(Capability::Read),
                zone.clone().prop_map(Capability::Write),
                zone.clone().prop_map(Capability::ReadSummary),
                // An MCP name: 1-20 alphanumeric chars.
                "[a-zA-Z0-9]{1,20}".prop_map(Capability::ExecuteMcp),
                // A tool name: 1-20 chars; the `:` allows the documented `"<mcp>:<tool>"` form.
                "[a-zA-Z0-9:]{1,20}".prop_map(Capability::ExecuteTool),
                Just(Capability::AskHuman),
            ]
            .boxed()
        }
    }

    impl Arbitrary for CapabilitySet {
        type Parameters = ();
        type Strategy = BoxedStrategy<CapabilitySet>;

        fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
            // A raw 0-10 element bag, wrapped as-is (no `FromIterator` de-dup, so order and
            // duplicates are part of what the properties see).
            prop::collection::vec(any::<Capability>(), 0..10)
                .prop_map(|capabilities| CapabilitySet { capabilities })
                .boxed()
        }
    }

    // ── Properties ──────────────────────────────────────────────────────────────────────────────

    /// `narrow` is commutative: the result must not depend on which set is the narrowing.
    fn narrow_commutative(a: CapabilitySet, b: CapabilitySet) -> bool {
        a.narrow(&b) == b.narrow(&a)
    }

    /// `narrow` is idempotent for a fixed second operand: narrowing an already-narrowed set by the
    /// same narrowing changes nothing.
    fn narrow_idempotent(a: CapabilitySet, b: CapabilitySet) -> bool {
        let n = a.narrow(&b);
        n.narrow(&b) == n
    }

    /// The Decision 4 invariant, per element: whatever survives a narrowing is subsumed by something
    /// on *both* sides — authority can only shrink, never be invented by the intersection.
    fn narrow_never_widens(a: CapabilitySet, b: CapabilitySet) -> bool {
        let result = a.narrow(&b);
        result.capabilities.iter().all(|c| {
            a.capabilities.iter().any(|aa| aa.subsumes(c))
                && b.capabilities.iter().any(|bb| bb.subsumes(c))
        })
    }

    /// `narrow` is associative. Note: the derived `PartialEq` compares `capabilities` as an ordered
    /// `Vec`, and `narrow` emits in pair-iteration order, so an ordering mismatch between the two
    /// evaluation orders can read as a failure even when the result *sets* agree. With the
    /// uncorrelated name strategy above this is only reachable via the equality arms of `subsumes`,
    /// under which associativity holds.
    fn narrow_associative(a: CapabilitySet, b: CapabilitySet, c: CapabilitySet) -> bool {
        a.narrow(&b).narrow(&c) == a.narrow(&b.narrow(&c))
    }

    proptest! {
        #[test]
        fn prop_narrow_commutative(a: CapabilitySet, b: CapabilitySet) {
            prop_assert!(narrow_commutative(a, b));
        }

        #[test]
        fn prop_narrow_idempotent(a: CapabilitySet, b: CapabilitySet) {
            prop_assert!(narrow_idempotent(a, b));
        }

        #[test]
        fn prop_narrow_never_widens(a: CapabilitySet, b: CapabilitySet) {
            prop_assert!(narrow_never_widens(a, b));
        }

        #[test]
        fn prop_narrow_associative(a: CapabilitySet, b: CapabilitySet, c: CapabilitySet) {
            prop_assert!(narrow_associative(a, b, c));
        }
    }
}
