//! Doom-loop and short-cycle guards: how a run decides "the model is thrashing" and what to
//! tell the model about it.
//!
//! Split out of `lib.rs` so the detection primitives ([`is_doom_loop`], [`detect_short_cycle`],
//! [`args_similarity`] and friends) live next to each other rather than scattered through the
//! run-loop impl. Public re-exports ([`ArgMatch`], [`LoopProfile`]) are kept in `lib.rs` so the
//! orchestrator and the coder-agent see no API change.
//!
//! `DOOM_LOOP_NUDGE` (the user-facing text) and the per-mechanism [`LoopGuard`] (the state the
//! run loop escalates) also live here — both are loop-guard concerns, not loop-execution
//! concerns. The escalation helpers ([`Escalation`], [`LoopGuard::strike`]) are private to this
//! module.

/// How strictly two consecutive same-tool calls must resemble each other to count as a repeat.
///
/// The semantic bar is right for acting work, where re-issuing a nearly-identical call is almost
/// always thrash. It is wrong for **search**: "orchestration anti-patterns" and "agentic AI
/// failure modes" are different queries that a bag-of-words comparison scores as near-duplicates,
/// and a live deep-research run was stopped three times for exactly that — legitimate query
/// variation read as a loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArgMatch {
    /// Near-duplicate arguments count as a repeat ([`ARG_SIMILARITY_THRESHOLD`]).
    #[default]
    Semantic,
    /// Only byte-identical arguments count. Re-running the *same* query still trips the guard —
    /// that is real thrash — but varied queries never do.
    Exact,
}

/// Per-task loop-detection settings. Separate from the resource budget because it tunes what
/// counts as a problem, not how much of the resource is left.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopProfile {
    pub arg_match: ArgMatch,
}

impl LoopProfile {
    /// The default: near-duplicate arguments count as a repeat.
    pub fn semantic() -> Self {
        Self {
            arg_match: ArgMatch::Semantic,
        }
    }

    /// For search-shaped work, where varied queries are the job rather than a symptom.
    pub fn exact() -> Self {
        Self {
            arg_match: ArgMatch::Exact,
        }
    }
}

/// The per-run behaviour `run_loop` needs from its task: how repeats are judged, and whether
/// partial work is worth filing. Grouped rather than passed as loose arguments — they travel
/// together and always come from the same place.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RunPolicy {
    pub(crate) salvageable: bool,
    pub(crate) loop_profile: LoopProfile,
}

/// The 3-step escalation ladder (1st -> nudge, 2nd -> remove, 3rd+ -> give up) for one
/// loop-detection mechanism. `run_loop` keeps one `LoopGuard` per mechanism (doom-loop,
/// short-cycle) rather than a single counter shared between them — an earlier version shared one
/// `loop_strikes: u8` across both, so whichever mechanism detected a problem *second* silently
/// skipped its own nudge step whenever the other had already struck once (e.g. a short cycle
/// nudging first meant the very next, entirely unrelated doom-loop detection jumped straight to
/// tool removal, never having nudged for that behavior at all). The one-time turn-budget top-up
/// (see `DOOM_LOOP_RECOVERY_BONUS_TURNS` in `lib.rs`) stays a single shared flag in `run_loop`,
/// since that grant is genuinely per-run, not per-mechanism.
#[derive(Default)]
pub(crate) struct LoopGuard {
    pub(crate) strikes: u8,
}

/// What a [`LoopGuard`] says to do in response to its mechanism detecting a problem again.
pub(crate) enum Escalation {
    Nudge,
    Remove,
    GiveUp,
}

impl LoopGuard {
    pub(crate) fn strike(&mut self) -> Escalation {
        self.strikes += 1;
        match self.strikes {
            1 => Escalation::Nudge,
            2 => Escalation::Remove,
            _ => Escalation::GiveUp,
        }
    }
}

/// The first, softest escalation step when the doom-loop guard fires — mirrors `REPORT_NUDGE`'s
/// nudge shape: engine-level, independent of whatever `DIRECT_INSTRUCTIONS`/`SUBAGENT_PREAMBLE`
/// text the tuner eventually settles on. If it fires again, the guard stops asking and starts
/// removing the offending tool instead (see the guard block in `run_loop`) — live testing showed
/// this alone doesn't change DeepSeek/Gemini's behavior (they repeated a 4th time anyway, with
/// zero visible acknowledgment of the nudge in their response content), so it's a first try,
/// not the whole mechanism.
pub(crate) const DOOM_LOOP_NUDGE: &str = "You've called the same tool with the same or very similar arguments \
several times in a row without new information. Use the result you already have to take the next \
step in the plan, or call `submit_report` if you're genuinely stuck — repeating that call again \
will not help.";

/// The first escalation step for the second failure shape this guard catches: alternating
/// between the same short cycle of tools (A, B, A, B, ...) instead of a repeated single call.
/// VTCode's `LoopDetector` calls this pattern out explicitly (`detect_patterns`) as distinct
/// from a single tool repeating — worth guarding even without live evidence of it happening yet,
/// since the detection is essentially free (exact tool-name matching over the same call history
/// this guard already tracks) and the underlying risk (burning the turn budget without progress)
/// is identical.
pub(crate) const CYCLE_NUDGE: &str = "You're alternating between the same short cycle of tools without making \
new progress. Break the cycle: use what you already have to take a genuinely different next step, \
or call `submit_report` if you're stuck.";

/// Minimum cosine similarity (see [`args_similarity`]) between consecutive same-tool calls'
/// arguments for them to count as "the same call" for `DOOM_LOOP_THRESHOLD` purposes.
/// Hand-calibrated against the two cases that matter, not a large corpus (still just a starting
/// point, revisit if live use shows false positives/negatives): the real DeepSeek transcript's 3
/// rephrasings of the same question scored ~0.26/~0.41/~0.24 pairwise, while 3 genuinely
/// distinct queries to the same tool ("weather in Denver" / "capital of France" / "current
/// bitcoin price") scored ~0.10. `0.2` sits between those clusters — closer to the
/// distinct-queries side, since a missed detection just costs one more turn before the next
/// check, while a false positive would nudge the model away from legitimately varied, on-track
/// work.
pub(crate) const ARG_SIMILARITY_THRESHOLD: f32 = 0.2;

/// How many consecutive, *near-duplicate* invocations of the same tool count as a "doom loop" —
/// the model succeeding at a tool call every time yet making no progress, rather than hitting
/// an error it could react to. Matches the threshold comparable harnesses use for the same
/// failure mode (opencode/kilocode's `DOOM_LOOP_THRESHOLD`, VTCode's `LoopDetector`) — evidence
/// this needs an engine-level guard, not just prompt wording, came from a live reproduction of
/// the deep-research reliability finding: DeepSeek and Gemini both got stuck calling `deepwiki`
/// 3-6 times in a row (every call succeeded; the result was just an unhelpful, repeatable
/// answer) and never reached the second required tool, burning the whole turn budget. A tool
/// call *succeeding* every time denies the model the one signal ("that failed") it reliably
/// adapts to; whether it *also* notices "repeating this won't help" is a subtler, less reliable
/// judgment call — and even a model that would eventually notice doesn't get the chance inside
/// Liberado's tight turn budgets (4 for `ExecuteDirect`).
///
/// "Near-duplicate" matters, not just "identical": a first cut of this guard checked
/// byte-for-byte argument equality and it did not fire against the real failure above, because
/// the model was rephrasing the same question each call (`"turbomcp transport layer"` ->
/// `"turbo-mcp transport Provider trait stdio HTTP JSON-RPC MCP protocol"` -> ...) rather than
/// repeating it verbatim. See [`args_similarity`].
pub(crate) const DOOM_LOOP_THRESHOLD: usize = 3;

/// Tools whose arguments *are* file content, and which therefore cannot be judged by similarity.
///
/// Two different edits to the same file are always textually alike: same `path`, same language,
/// overlapping identifiers, often overlapping lines. [`args_similarity`] scores that pair high
/// and is not wrong to — it is measuring "same file", which for a search tool is a good proxy
/// for "same action" and for an edit tool is no proxy at all. Editing one file repeatedly is
/// what applying a change looks like.
///
/// Measured, not assumed. In an A/B on 2026-08-11 the coding pack made four consecutive
/// `edit_file` calls — one moving a test helper, one fixing an assertion, two adding tests,
/// across two files — and the guard withdrew `edit_file` on the next turn. It lost `apply_patch`
/// and `run_command` later the same way, and the run ended with the model saying it knew which
/// two call sites were broken and had no tool left to fix them. Kilo Code made 36 edits on the
/// same task, was never disarmed, and shipped a clean pass.
///
/// For these tools only an identical call counts as a repeat. That still catches the real
/// pathology — replaying the byte-identical edit achieves nothing however many times you send it —
/// while a different edit is progress by definition.
pub(crate) fn arguments_are_file_content(tool: &str) -> bool {
    matches!(
        tool,
        "edit_file"
            | "write_file"
            | "apply_patch"
            | "edit"
            | "write"
            | "patch"
            | "multiedit"
            // `run_command` is many programs under one name. Semantic similarity
            // on `rg` + a shared path withdrew it on compare 7 after three
            // different searches. Identical replay is still a doom loop.
            | "run_command"
            | "run_command_background"
            | "bash"
            | "exec"
    )
}

/// Whether two calls of `tool` count as the same action.
///
/// Inspect tools follow `profile`: same path is the same look. File-content
/// tools ([`arguments_are_file_content`]) require byte-identical arguments —
/// two edits of the same file with different `old`/`new` are progress;
/// replaying the same edit is not.
pub(crate) fn arguments_repeat(
    tool: &str,
    a: &serde_json::Value,
    b: &serde_json::Value,
    profile: LoopProfile,
) -> bool {
    let kind = if arguments_are_file_content(tool) {
        ArgMatch::Exact
    } else {
        profile.arg_match
    };
    match kind {
        ArgMatch::Exact => a == b,
        ArgMatch::Semantic => args_similarity(a, b) >= ARG_SIMILARITY_THRESHOLD,
    }
}

/// Whether the last [`DOOM_LOOP_THRESHOLD`] invocations are consecutively the same tool, called
/// with near-duplicate arguments (see [`args_similarity`]) — see [`DOOM_LOOP_THRESHOLD`]'s doc
/// comment for why near-duplicate, not just byte-identical, is the right bar.
pub(crate) fn is_doom_loop(
    history: &[(String, serde_json::Value, String)],
    profile: LoopProfile,
) -> bool {
    let Some((last_name, ..)) = history.last() else {
        return false;
    };
    // Most-recent-first, stopping at the first call that isn't consecutively the same tool.
    let streak: Vec<&serde_json::Value> = history
        .iter()
        .rev()
        .take_while(|(name, ..)| name == last_name)
        .map(|(_, args, _)| args)
        .collect();
    if streak.len() < DOOM_LOOP_THRESHOLD {
        return false;
    }
    streak[..DOOM_LOOP_THRESHOLD]
        .windows(2)
        .all(|pair| arguments_repeat(last_name, pair[0], pair[1], profile))
}

/// Whether the tail of `history` is a short repeating cycle (period 2 or 3 — e.g. A,B,A,B or
/// A,B,C,A,B,C) *over the same arguments*. Returns the distinct tool names participating in the
/// cycle (so the caller can remove exactly those, not the whole catalog) rather than a bare
/// bool.
///
/// Matching tool names is necessary but **not** sufficient. `read_file(a)`, `search_text(x)`,
/// `read_file(b)`, `search_text(y)` is what reading an unfamiliar codebase looks like: the names
/// alternate, but every call names a different resource and every call makes progress. Requiring
/// the positionally-corresponding arguments to repeat — inspect slots via [`args_similarity`]
/// and [`IDENTITY_ARG_KEYS`], file-content slots via exact args (see [`arguments_repeat`]) —
/// separates that from genuine thrash. Same-file `read`/`edit` with a different `old`/`new` is
/// the mandated coding loop, not a cycle; replaying the same edit is.
///
/// This was not academic: with names-only matching the guard fired on turn 4 of a 60-turn coding
/// run, removed `read_file`/`search_text` for the rest of the task, and the model filed a
/// complete implementation plan it had no remaining way to carry out ("blocked from making edits
/// by the progress guard"). Period 2 needs only four calls, so *any* task requiring more than
/// four alternating inspections was unreachable.
///
/// A mono-tool streak (`read_note`×4 in one parallel batch) is **not** a cycle — period-2 would
/// match `AAAA` as two copies of `AA`, which is a false positive that used to mid-batch-nudge
/// and leave unanswered `tool_call_id`s (dogfood session `01KX7BWV`). Same-tool thrash is
/// [`is_doom_loop`]'s job.
pub(crate) fn detect_short_cycle(
    history: &[(String, serde_json::Value, String)],
) -> Option<Vec<String>> {
    for period in 2..=3 {
        let window = period * 2;
        if history.len() < window {
            continue;
        }
        let tail = &history[history.len() - window..];
        let (first_half, second_half) = tail.split_at(period);
        // Same tool in the same slot of both halves...
        if !first_half
            .iter()
            .zip(second_half)
            .all(|((a_name, ..), (b_name, ..))| a_name == b_name)
        {
            continue;
        }
        // ...called on the same thing. Inspect slots use path identity;
        // file-content slots (edit/write/`run_command`) require identical
        // arguments. Same-file read → edit → read → edit is the mandated
        // loop when the edits differ; replaying the same edit is a cycle.
        if !first_half
            .iter()
            .zip(second_half)
            .all(|((a_name, a_args, _), (_, b_args, _))| {
                arguments_repeat(a_name, a_args, b_args, LoopProfile::semantic())
            })
        {
            continue;
        }
        let mut distinct: Vec<String> = first_half.iter().map(|(name, ..)| name.clone()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        // Require a real multi-tool pattern, not "the same tool N times in a row".
        if distinct.len() < 2 {
            continue;
        }
        return Some(distinct);
    }
    None
}

/// Object keys whose string values name a distinct resource. If both calls set the same key to
/// *different* strings, the calls are not near-duplicates — bag-of-words alone would still score
/// `{"path":"Tasks/A.md"}` vs `{"path":"Tasks/B.md"}` high (shared `path` / `tasks` / `md` tokens)
/// and false-positive doom-loop on legitimate parallel multi-file reads (dogfood `01KX7BWV`).
pub(crate) const IDENTITY_ARG_KEYS: &[&str] = &["path", "file", "filepath", "note", "uri", "id"];

/// Cosine similarity between two tool calls' arguments, weighted so a term shared by both calls
/// (boilerplate, or the topic every rephrasing shares) counts for less than a term unique to one
/// side — see [`DOOM_LOOP_THRESHOLD`]'s doc comment for why byte-equality alone missed the real
/// failure this guards against. `1.0` when both sides tokenize to nothing (e.g. both `{}`): with
/// no text to compare, equality of the raw value is the only signal left. Deterministic, local,
/// no network/model call — a small bag-of-words IDF over just the two documents being compared,
/// not a learned embedding.
///
/// Before TF-IDF: if both args carry an [`IDENTITY_ARG_KEYS`] field and the values differ, return
/// `0.0` immediately (distinct resources ⇒ not a doom loop).
pub(crate) fn args_similarity(a: &serde_json::Value, b: &serde_json::Value) -> f32 {
    if identity_args_conflict(a, b) {
        return 0.0;
    }
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return if a == b { 1.0 } else { 0.0 };
    }
    let vectors = tf_idf_vectors(&[tokens_a, tokens_b]);
    cosine(&vectors[0], &vectors[1])
}

/// True when both objects set the same identity key to different string values.
pub(crate) fn identity_args_conflict(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    for key in IDENTITY_ARG_KEYS {
        match (
            a.get(*key).and_then(|v| v.as_str()),
            b.get(*key).and_then(|v| v.as_str()),
        ) {
            (Some(x), Some(y)) if x != y => return true,
            _ => {}
        }
    }
    false
}

/// Lowercased alphanumeric runs from a JSON value's textual form — deliberately crude (no
/// stemming/stopwords), adequate for comparing short tool-call argument strings.
pub(crate) fn tokenize(value: &serde_json::Value) -> Vec<String> {
    value
        .to_string()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// TF-IDF vectors for a small set of tokenized documents, IDF computed from just that set
/// (there's no larger corpus available or wanted here — see [`args_similarity`]).
pub(crate) fn tf_idf_vectors(docs: &[Vec<String>]) -> Vec<std::collections::HashMap<String, f32>> {
    let n = docs.len() as f32;
    let mut doc_freq: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for doc in docs {
        let unique: std::collections::HashSet<&str> = doc.iter().map(String::as_str).collect();
        for term in unique {
            *doc_freq.entry(term).or_insert(0) += 1;
        }
    }
    docs.iter()
        .map(|doc| {
            let mut tf: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
            for term in doc {
                *tf.entry(term.clone()).or_insert(0.0) += 1.0;
            }
            for (term, count) in tf.iter_mut() {
                let df = *doc_freq.get(term.as_str()).unwrap_or(&1) as f32;
                // +1 smoothing: a term every doc shares still contributes a little (so two fully
                // identical documents still cosine to 1.0), rather than vanishing entirely.
                *count *= (n / df).ln() + 1.0;
            }
            tf
        })
        .collect()
}

pub(crate) fn cosine(
    a: &std::collections::HashMap<String, f32>,
    b: &std::collections::HashMap<String, f32>,
) -> f32 {
    let dot: f32 = a
        .iter()
        .map(|(term, weight)| weight * b.get(term).copied().unwrap_or(0.0))
        .sum();
    let norm_a = a.values().map(|v| v * v).sum::<f32>().sqrt();
    let norm_b = b.values().map(|v| v * v).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    // f32 summation order can nudge a true 1.0 a hair past the unit interval (see
    // proptest_args_similarity_stays_in_unit_range / CI on main after #208). Clamp so
    // callers treating this as a [0, 1] similarity never see out-of-range scores.
    (dot / (norm_a * norm_b)).clamp(0.0, 1.0)
}

#[cfg(test)]
#[path = "loop_guard_similarity_tests.rs"]
mod similarity_tests;
