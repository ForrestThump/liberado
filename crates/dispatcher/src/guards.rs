//! The deterministic guard pipeline (`liberado-dispatch-logic-spec.md` §6).
//!
//! Guards run **after** the classifier, in pure code, and can only move a decision toward *less*
//! autonomy — never more. This is what makes the right behavior emergent and the wrong behavior
//! cheap: a misclassification can waste tokens, but it cannot escalate past a guard into an
//! unsafe action. Because the guards are deterministic, the entire safety surface is exactly
//! assertable (Decision 16) — only the classifier's *quality* is probabilistic, never its safety.
//!
//! Scope: capability, consequence-gate, zone-write-class, magnitude, reaction-depth, and
//! confidence-floor guards. (This comment previously listed the consequence gate, then the
//! zone-write-class gate, as deferred — stale as of whenever each actually shipped; the code below
//! has implemented both for a while by the time you're reading this.)
//!
//! ## Keeping this in sync with the runtime guard (`liberado-executor`'s `RiskGatedToolRuntime`)
//!
//! This is the pre-flight half of a two-part safety net; `RiskGatedToolRuntime` is the runtime half
//! that applies the equivalent checks to every *adaptive* (non-seed) call, since a call this guard
//! never saw at dispatch time still has to pass something before it runs. The zone-write-class
//! check is unified (`liberado_common::zone_write_restriction`) so it can't drift between the two.
//! The capability/consequence/magnitude checks are NOT unified — different shapes at each site for
//! good reason — but if you add a **new** guard here, check whether `risk_gated.rs` needs the
//! runtime equivalent, and vice versa.

use liberado_common::CONSEQUENCE_GATE;
use liberado_common::{
    BlockReason, Consequence, DispatchAction, DispatchDecision, bare_tool_name, instruction_scope,
    is_sweeping_destructive, mcp_of, write_target, zone_write_restriction,
};
use liberado_config_loader::DispatchTuning;

use crate::{DispatchRequest, McpDescriptor};

/// The precise guard that rejected a classified action. `BlockReason` is intentionally coarser
/// for the public wire format, but approval continuation needs to know which one-time runtime
/// exception the human authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuardKind {
    AskHumanCapability,
    McpGrant,
    Consequence,
    ZoneWriteClass,
    Magnitude,
    ReactionDepth,
    ConfidenceFloor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuardViolation {
    pub guard: GuardKind,
    pub reason: BlockReason,
}

/// Evaluate the guards against a classified decision. Returns the [`BlockReason`] of the first
/// (highest-priority) violation, or `None` if the decision passes unchanged. The caller downgrades
/// to a `Clarify` carrying this reason.
///
/// Priority order — most fundamental first, so the reported reason is the most actionable:
/// capability gap → reaction-depth limit → confidence floor.
pub fn evaluate(
    decision: &DispatchDecision,
    req: &DispatchRequest,
    tuning: &DispatchTuning,
    max_reaction_depth: u32,
) -> Option<BlockReason> {
    evaluate_detailed(decision, req, tuning, max_reaction_depth).map(|violation| violation.reason)
}

pub(crate) fn evaluate_detailed(
    decision: &DispatchDecision,
    req: &DispatchRequest,
    tuning: &DispatchTuning,
    max_reaction_depth: u32,
) -> Option<GuardViolation> {
    // A Clarify is the most conservative action — *when somebody can answer it*.
    //
    // For an actor holding no `AskHuman` capability there is no one, so a question is not a
    // conservative fallback, it is a dead end: the run is spent producing something delivered to
    // nobody. The capability model already says this ("structurally unable to block on a person who
    // isn't there") and the homelab's `dispatcher` grant already omits `AskHuman` — this guard is
    // the dispatcher finally reading a constraint that was declared all along, rather than a new
    // rule. A live evening-debrief cron burned a run on "how should I proceed?" at 01:55.
    if matches!(decision.action, DispatchAction::Clarify { .. }) {
        if !req.capabilities.grants_ask_human() {
            return blocked(
                GuardKind::AskHumanCapability,
                BlockReason::Unattended,
                "Clarify requires an interlocutor and this actor holds no AskHuman capability",
            );
        }
        return None;
    }

    // (1) Capability check — never auto-widen (Decision 4 invariant). Everything the action would
    // invoke must be granted in the active capability set.
    //
    // Checked at whatever precision the reference carries: a seed call names a concrete
    // `<mcp>:<tool>`, so it is checked against the tool-level grant; a `relevant_mcps` /
    // `allowed_mcps` hint names only a server, so only the server can be checked. Collapsing seed
    // calls to their MCP (which this used to do) would pass a partial grant's *ungranted* tools
    // pre-flight and leave the refusal to `RiskGatedToolRuntime` — safe, since that gate does ask the
    // tool-level question, but it burns a dispatch turn to arrive at an error we could name here.
    for reference in referenced_grants(&decision.action) {
        let authorized = match reference.contains(':') {
            true => req.capabilities.grants_tool(reference),
            false => req.capabilities.grants_mcp(reference),
        };
        if !authorized {
            tracing::warn!(
                reference,
                action = %decision.action,
                "capability gap: action references something not in the dispatcher grant"
            );
            return blocked(
                GuardKind::McpGrant,
                BlockReason::CapabilityGap,
                &format!("action references '{reference}', which the grant does not include"),
            );
        }
    }

    // (2) Consequence gate (§6 #3) — a permitted action that would touch something irreversible or
    // external (an email/message, an unversioned delete) needs human confirmation, even at high
    // confidence. A git-tracked vault write is `Reversible` and passes; `External`/`Irreversible`
    // does not.
    let consequence = max_consequence(&decision.action, req);
    if consequence >= CONSEQUENCE_GATE {
        return blocked(
            GuardKind::Consequence,
            BlockReason::HighConsequence,
            &format!("an MCP in scope is rated {consequence:?} (gate {CONSEQUENCE_GATE:?})"),
        );
    }

    // (2b) Zone-write-class gate (§6 #2) — a permitted, low-*general*-consequence action can still
    // target a *specific* vault zone the human has restricted (`proposal_only`/`human_only`).
    // Pre-flight scope only (this checks `ExecuteDirect`'s seed calls — see `zone_restricted`'s own
    // doc comment); the real, always-enforced boundary for every call including adaptive ones is
    // `RiskGatedToolRuntime`.
    if zone_restricted(&decision.action, req) {
        return blocked(
            GuardKind::ZoneWriteClass,
            BlockReason::ZoneRestricted,
            "a seed call targets a zone that is not directly agent-writable",
        );
    }

    // (3) Magnitude gate — a *sweeping destructive* action is high-stakes by reach even when each
    // change is reversible ("delete all my notes" in a git-tracked vault). Read from the goal: it's
    // the tool-independent signal available pre-execution, and (unlike a specific tool name) it
    // survives the model routing the work to a subagent. Liberado owns this classification because
    // MCP tools don't declare their own risk. Per-call, args-aware enforcement is a later layer.
    // Scoped to the *instruction* — a goal that merely narrates a past deletion in a trailing
    // `Context:` section is not asking for one. See `instruction_scope`'s doc comment for the
    // live false positive that motivated this.
    //
    // Waiver: a `[[risk_waivers]]` entry that covers every (tool, zone) this action would touch
    // suppresses this gate. Waivers do not grant authority — that is the capability check above;
    // they only say "for these tool calls, the magnitude heuristic adds no safety beyond the
    // structural checks already run." Today the typical waiver covers a read-only MCP wholesale
    // or a path-addressed read tool in zones the agent routinely fetches.
    let instruction = instruction_scope(&req.goal);
    if is_sweeping_destructive(instruction) {
        let targets = magnitude_targets(&decision.action, &req.catalog);
        let waived = targets.iter().all(|(tool, zone)| {
            req.risk_waivers
                .covers(liberado_common::Guard::Magnitude, tool, zone.as_deref())
        }) && !targets.is_empty();
        if !waived {
            return blocked(
                GuardKind::Magnitude,
                BlockReason::HighConsequence,
                &format!(
                    "instruction reads as sweeping+destructive ({} of {} goal chars scanned)",
                    instruction.len(),
                    req.goal.len()
                ),
            );
        }
        tracing::info!(
            targets = ?targets,
            "magnitude gate suppressed by risk waiver — instruction matched but every target is waived"
        );
    }

    // (4) Reaction-depth guard — halt runaway background cascades.
    if req.reaction_depth >= max_reaction_depth {
        return blocked(
            GuardKind::ReactionDepth,
            BlockReason::DepthLimit,
            &format!("depth {} >= max {max_reaction_depth}", req.reaction_depth),
        );
    }

    // (5) Confidence floor — below the bar, ask rather than act. The write threshold is applied
    // conservatively to any action-taking decision (read/write tiering needs per-tool metadata,
    // deferred); `Clarify` was already excluded above.
    if decision.confidence < tuning.clarify_threshold_write {
        return blocked(
            GuardKind::ConfidenceFloor,
            BlockReason::LowConfidence,
            &format!(
                "confidence {:.2} < threshold {:.2}",
                decision.confidence, tuning.clarify_threshold_write
            ),
        );
    }

    None
}

/// Name the guard that just fired, alongside the [`BlockReason`] it produces.
///
/// `BlockReason` is a coarse wire type deliberately shared by more than one check: the consequence
/// gate and the magnitude gate both return `HighConsequence`, and they are different problems with
/// different fixes. When a live proposal appeared with `downgrade=HighConsequence`, telling the two
/// apart required re-running the heuristic offline against the goal text — the log could not answer
/// it. `guard=` closes that, and matches the field name `RiskGatedToolRuntime::authority_decision`
/// uses on the runtime side, so one grep covers both enforcement points.
fn blocked(guard: GuardKind, reason: BlockReason, detail: &str) -> Option<GuardViolation> {
    tracing::warn!(guard = ?guard, ?reason, detail = %detail, "pre-flight guard blocked the action");
    Some(GuardViolation { guard, reason })
}

/// The MCPs an action would invoke. The tool-name convention is `"<mcp>:<tool>"`; a bare name is
/// treated as the MCP itself.
/// Everything an action would invoke, each at the precision the action states it: a qualified
/// `"<mcp>:<tool>"` for a concrete seed call, a bare MCP name for a scope hint.
///
/// The caller distinguishes them by the `:` and asks the matching question. Kept as one list because
/// the guard's job is "is all of this granted", and splitting it would invite checking one list and
/// forgetting the other.
fn referenced_grants(action: &DispatchAction) -> Vec<&str> {
    match action {
        // Pre-flight check over the classifier's opening move (`seed_calls`) AND its narrowing hint
        // (`relevant_mcps`, if the model populated one) — a hallucinated or out-of-scope name in
        // either gets caught here, the same capability-gap protection `DispatchSubagent.allowed_mcps`
        // already gets below. The real boundary is still runtime: the executor only offers tools the
        // capability set permits, so an adaptive call it makes later is enforced there too, even
        // though it isn't visible to this pre-flight guard.
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            ..
        } => seed_calls
            // The tool name whole, not `mcp_of` it: this is the one place the action is specific
            // enough to check a per-tool grant, and discarding that was the precision loss.
            .iter()
            .map(|c| c.tool.as_str())
            .chain(relevant_mcps.iter().map(String::as_str))
            .collect(),
        DispatchAction::DispatchSubagent { allowed_mcps, .. } => {
            allowed_mcps.iter().map(String::as_str).collect()
        }
        // Clarify carries no calls; Propose is a post-guard output the guards never receive.
        DispatchAction::Clarify { .. } | DispatchAction::Propose { .. } => Vec::new(),
    }
}

/// The highest consequence among the MCPs an action would touch, looked up from the catalog. An MCP
/// the catalog doesn't describe contributes nothing (`ReadOnly`). Like the capability check, this is
/// a pre-flight read of the action's declared scope; runtime gating of an `ExecuteDirect`'s adaptive
/// calls is a separate, later boundary.
fn max_consequence(action: &DispatchAction, req: &DispatchRequest) -> Consequence {
    referenced_grants(action)
        .into_iter()
        // `mcp_of` is mandatory here, not cosmetic: consequence is declared per MCP, and
        // `referenced_grants` now yields qualified `<mcp>:<tool>` names for seed calls. Comparing
        // those against `d.name` matches nothing, and an unmatched entry contributes
        // `Consequence::ReadOnly` — so omitting this would quietly disarm the consequence gate for
        // every `ExecuteDirect`, the exact actions it exists to catch.
        .map(mcp_of)
        .filter_map(|mcp| {
            req.catalog
                .iter()
                .find(|d| d.name == mcp)
                .map(|d| d.consequence)
        })
        .max()
        .unwrap_or_default()
}

/// Whether any of `action`'s seed calls target a zone whose declared `WriteClass` doesn't allow a
/// direct agent write (`ProposalOnly`/`HumanOnly`) — the zone-write-class guard (§6 #2).
///
/// Only `ExecuteDirect`'s seed calls are checked — the same pre-flight scope `max_consequence`
/// above already accepts: a `DispatchSubagent`'s adaptive calls aren't known yet at dispatch time,
/// and this is a check, not the boundary (`RiskGatedToolRuntime` is, for every call including
/// adaptive ones — see `liberado_common::zone_write_restriction`'s own doc comment, the shared
/// determination logic both this guard and that runtime call so the two can't silently drift
/// apart on what counts as restricted).
fn zone_restricted(action: &DispatchAction, req: &DispatchRequest) -> bool {
    let DispatchAction::ExecuteDirect { seed_calls, .. } = action else {
        return false;
    };
    seed_calls.iter().any(|call| {
        zone_write_restriction(
            mcp_of(&call.tool),
            bare_tool_name(&call.tool),
            &req.catalog,
            &req.zone_write_classes,
        )
        .is_some()
    })
}

/// Per-call resolution the magnitude gate consults: each entry is `(tool, zone)`. The
/// `String`s are owned so a closure-built iterator can yield borrows outliving it. The
/// caller turns these into [`liberado_common::WaiverTarget`]s.
fn magnitude_targets(
    action: &DispatchAction,
    catalog: &[McpDescriptor],
) -> Vec<(String, Option<String>)> {
    match action {
        DispatchAction::ExecuteDirect { seed_calls, .. } => seed_calls
            .iter()
            .map(|call| {
                let mcp = mcp_of(&call.tool);
                let bare = bare_tool_name(&call.tool);
                // Resolve the call's target zone against the catalog. `WriteTarget::Zone(name)`
                // is the only branch that produces a Some(zone); reads and undeterminable writes
                // both yield None so a zone-restricted waiver does not accidentally match.
                let zone = catalog
                    .iter()
                    .find(|d| d.name == mcp)
                    .and_then(|d| match write_target(d, bare, &call.args) {
                        liberado_common::WriteTarget::Zone(name) => Some(name),
                        _ => None,
                    });
                (call.tool.clone(), zone)
            })
            .collect(),
        DispatchAction::DispatchSubagent { allowed_mcps, .. } => {
            allowed_mcps.iter().map(|mcp| (mcp.clone(), None)).collect()
        }
        DispatchAction::Clarify { .. } | DispatchAction::Propose { .. } => Vec::new(),
    }
}

#[cfg(test)]
#[path = "guards_tests.rs"]
mod tests;
