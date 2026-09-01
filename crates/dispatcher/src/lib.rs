//! # liberado-dispatcher
//!
//! The out-of-band router (Decision 1, `liberado-dispatch-logic-spec.md`). It takes a goal +
//! minimal context and produces a typed [`DispatchDecision`] — `ExecuteDirect`,
//! `DispatchSubagent`, or `Clarify` — via a small pipeline:
//!
//! 1. **classify** — one structured-output inference (temperature 0) turns the goal + MCP catalog
//!    into a candidate decision.
//! 2. **guard** — the deterministic, downgrade-only pipeline ([`guards`]) makes safety emergent:
//!    a misclassification can be wasteful but never unsafe.
//!
//! This slice produces the *decision*; it does not execute it (no MCP clients yet) and does not
//! yet consult procedural memory (the retrieve/record loop, deferred). Those are clean seams.
//!
//! Correctness is engineered, not hoped for: classification is the only probabilistic step, and
//! it is bounded on both sides — malformed output degrades to a safe `Clarify` rather than
//! crashing (Decision 13), and the guards constrain the result regardless of what the model said.

pub mod guards;

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use liberado_common::{
    BlockReason, CapabilitySet, Delivery, DispatchAction, DispatchDecision, GuidanceHit,
    ProposedAction, ToolCall, ToolGuidanceSource, mcp_of,
};
use liberado_config_loader::DispatchTuning;
use liberado_provider::{CompletionRequest, Message, Provider, ProviderError, complete_json};
use thiserror::Error;
use tracing::Instrument;

// Re-exported so existing `liberado_dispatcher::McpDescriptor` import paths (daemon, eval, this
// crate's own `guards` module) keep working unchanged — it's the same type `liberado_common`'s
// live `CapabilityCatalog` uses now, not a separate one. This crate used to define its own
// `McpDescriptor` (name/description/consequence, no `provenance`), duplicating
// `liberado_common::McpDescriptor` for a dependency-weight reason that no longer applied (this
// crate already depends on `liberado_common` broadly).
pub use liberado_common::McpDescriptor;

/// What the dispatcher needs to route a single goal. The dispatcher deliberately runs with *less*
/// context than the main agent (disjoint partitions); this is everything it sees.
#[derive(Clone, Debug)]
pub struct DispatchRequest {
    /// The restated, self-contained goal.
    pub goal: String,
    /// The MCP catalog the classifier may choose from (names + short descriptions only).
    pub catalog: Vec<McpDescriptor>,
    /// The capabilities currently in force — the ceiling the guards check against (never widened).
    pub capabilities: CapabilitySet,
    /// Correlation-chain depth: 0 for a user-initiated dispatch, >0 for a background reaction.
    pub reaction_depth: u32,
    /// `(zone, write_class)` pairs from `Policy.zones` (Decision 11) — what the zone-write-class
    /// guard (§6 #2) checks a seed call's resolved target zone against. A zone not present here
    /// falls back to the same conservative default `Policy::write_class` itself uses
    /// (`WriteClass::ProposalOnly`) — see `guards::zone_write_class`.
    pub zone_write_classes: Vec<(String, liberado_common::WriteClass)>,
    /// Declarative risk waivers loaded from `policy.toml` — used by the magnitude guard to
    /// suppress itself for matching tool calls. Defaults to empty (the unchanged pre-feature
    /// behaviour). Set this from `Policy::risk_waiver_set` at the bootstrap site so the guard
    /// pipeline and the runtime guard (`RiskGatedToolRuntime`) see the same set.
    pub risk_waivers: liberado_common::RiskWaiverSet,
}

/// Errors that abort a dispatch. Malformed model output does **not** appear here — it is handled
/// internally by degrading to a safe `Clarify`. Only a genuine provider failure (transport, rate
/// limit) propagates.
#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("provider failure during classification: {0}")]
    Provider(ProviderError),
}

/// The out-of-band dispatcher.
pub struct Dispatcher {
    provider: Arc<dyn Provider>,
    tuning: DispatchTuning,
    max_reaction_depth: u32,
    system_prompt: String,
    guidance: Option<Arc<dyn ToolGuidanceSource>>,
}

impl Dispatcher {
    /// Build a dispatcher over `provider`, with dispatch tunables and the reaction-depth cap
    /// (owned by the concurrency config — passed in so the two configs stay distinct).
    pub fn new(
        provider: Arc<dyn Provider>,
        tuning: DispatchTuning,
        max_reaction_depth: u32,
    ) -> Self {
        Self {
            provider,
            tuning,
            max_reaction_depth,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            guidance: None,
        }
    }

    /// Override the system prompt used during classification. Defaults to
    /// [`DEFAULT_SYSTEM_PROMPT`]. The heuristics tuner (`docs/future-work/heuristics-tuning-engine-plan.md`)
    /// uses this to test candidate prompts against the real dispatch code path without
    /// reimplementing it.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = prompt.into();
        self
    }

    /// Consult `guidance` (procedural memory) before classification, and let it record outcomes
    /// via [`Dispatcher::record_outcome`] (`liberado-dispatch-logic-spec.md` §2, steps 1/5).
    /// Optional — a `Dispatcher` without one behaves exactly as before this existed.
    pub fn with_guidance(mut self, guidance: Arc<dyn ToolGuidanceSource>) -> Self {
        self.guidance = Some(guidance);
        self
    }

    /// Route a goal to a guarded [`DispatchDecision`].
    pub async fn dispatch(&self, req: &DispatchRequest) -> Result<DispatchDecision, DispatchError> {
        let goal_hash = goal_hash(&req.goal);
        let reaction_depth = req.reaction_depth;

        let span = tracing::info_span!(
            "dispatch",
            goal_hash,
            reaction_depth,
            model = %self.provider.model(),
            action = tracing::field::Empty,
            confidence = tracing::field::Empty,
            downgrade = tracing::field::Empty,
        );

        async {
            let hits = self.retrieve_guidance(&req.goal).await;

            let mut classified = match self.guidance_short_circuit(&hits) {
                Some(decision) => decision,
                None => self.classify(req, &hits).await?,
            };
            ensure_correlation(&mut classified, goal_hash);
            enforce_narrow_direct_tools(&mut classified, self.tuning.narrow_direct_tools);
            // Drop / normalize invented MCP names before the capability guard (e.g. bare
            // `list_tasks` or `turbovault:list_tasks` as an "MCP name") so vault goals don't
            // false-CapabilityGap when turbovault is actually granted (dogfood 01KX9S39).
            sanitize_decision_mcps(&mut classified, &req.catalog);
            log_classified_decision(&classified, &self.provider.model());

            let decision =
                match guards::evaluate(&classified, req, &self.tuning, self.max_reaction_depth) {
                    Some(reason) => {
                        tracing::Span::current().record("downgrade", format_args!("{reason:?}"));
                        tracing::info!(
                            classified = %classified.action,
                            confidence = classified.confidence,
                            model = %self.provider.model(),
                            "dispatch decision downgraded by guard"
                        );
                        downgrade(classified, reason)
                    }
                    None => {
                        tracing::info!(
                            action = %classified.action,
                            confidence = classified.confidence,
                            model = %self.provider.model(),
                            "dispatch decision"
                        );
                        classified
                    }
                };

            tracing::Span::current().record("action", format_args!("{}", decision.action.label()));
            tracing::Span::current().record("confidence", decision.confidence);

            Ok(decision)
        }
        .instrument(span)
        .await
    }

    /// The classification step: one structured-output inference at temperature 0. Malformed or
    /// empty output is treated like very low confidence and degraded to a safe `Clarify`
    /// (Decision 13 resilience); a real provider failure propagates. `hits` (possibly empty)
    /// grounds the prompt with retrieved procedural-memory guidance — never narrows the catalog.
    async fn classify(
        &self,
        req: &DispatchRequest,
        hits: &[GuidanceHit],
    ) -> Result<DispatchDecision, DispatchError> {
        let span = tracing::info_span!(
            "classify",
            model = %self.provider.model(),
            provider = %self.provider.model(), // alias kept for existing greps
        );

        async {
            let request = self.build_request(req, hits);
            match complete_json::<dyn Provider, DispatchDecision>(
                self.provider.as_ref(),
                request,
                decision_schema(),
            )
            .await
            {
                Ok(decision) => {
                    tracing::debug!(
                        action = %decision.action,
                        confidence = decision.confidence,
                        "classification succeeded"
                    );
                    Ok(decision)
                }
                // The model responded, but unusably → safe default, don't crash.
                //
                // Logged WITH the reason (which now carries a prefix of what the model actually
                // said). A bare "unusable output" is unactionable: it cannot distinguish a provider
                // hiccup from a prompt the model refused to answer in JSON, and the difference is
                // the whole diagnosis. A failed evening-debrief cron was undiagnosable for exactly
                // this reason — the fallback is deliberately silent about a Clarify nobody can
                // answer, so this warning is the only trace the run leaves.
                Err(e @ (ProviderError::Decode(_) | ProviderError::EmptyResponse)) => {
                    tracing::warn!(
                        error = %e,
                        goal = %req.goal.chars().take(120).collect::<String>(),
                        "classification produced unusable output; degrading to Clarify"
                    );
                    Ok(clarify_fallback())
                }
                // A genuine provider failure — let the caller decide (retry/backoff).
                Err(e) => {
                    tracing::error!(error = %e, "classification failed");
                    Err(DispatchError::Provider(e))
                }
            }
        }
        .instrument(span)
        .await
    }

    fn build_request(&self, req: &DispatchRequest, hits: &[GuidanceHit]) -> CompletionRequest {
        let catalog = req
            .catalog
            .iter()
            .map(|m| format!("- {}: {}", m.name, m.description))
            .collect::<Vec<_>>()
            .join("\n");
        // Ordered stable-first so prefix caching has something to match: catalog and vault zones
        // are identical on every call for a given deployment, while the goal and the guidance
        // retrieved *for* that goal change every time. Anything varying that appears earlier
        // poisons the prefix for everything after it.
        let mut user_message = format!("Available MCPs:\n{catalog}");
        // The zones a `delivery = Vault` path may start with. Without this the classifier is being
        // asked to name a destination it has never been shown, so it invents a plausible one
        // (`research/`), the orchestrator's zone guard refuses the undeclared zone, and delivery
        // silently falls back — the feature would look broken rather than unconfigured. Only
        // directly-writable zones are listed: a `ProposalOnly`/`HumanOnly` zone would be refused
        // just the same, so offering it would only invite the mistake.
        let writable: Vec<&str> = req
            .zone_write_classes
            .iter()
            .filter(|(_, class)| class.allows_direct_agent_write())
            .map(|(zone, _)| zone.as_str())
            .collect();
        if !writable.is_empty() {
            user_message.push_str(&format!(
                "\n\nVault zones a `delivery` path may start with (exact, case-sensitive):\n{}",
                writable.join(", ")
            ));
        }
        // Varying from here down: the goal, then the guidance retrieved for it.

        // Varying from here down: the goal, then the guidance retrieved for it.
        user_message.push_str(&format!(
            "

Goal:
{}",
            req.goal
        ));
        if !hits.is_empty() {
            let guidance = hits
                .iter()
                .map(|h| format!("- {}", h.content))
                .collect::<Vec<_>>()
                .join("\n");
            user_message.push_str(&format!(
                "\n\nRelevant past guidance (may or may not apply — use your judgement):\n{guidance}"
            ));
        }
        CompletionRequest::new(vec![
            Message::system(&self.system_prompt),
            Message::user(user_message),
        ])
        .with_temperature(0.0)
    }

    /// RETRIEVE (`liberado-dispatch-logic-spec.md` §2 step 1): consult procedural memory before
    /// classification. Empty when no guidance source is configured, or on any backend failure —
    /// retrieval is a hint, never load-bearing enough to abort a dispatch over.
    async fn retrieve_guidance(&self, goal: &str) -> Vec<GuidanceHit> {
        match &self.guidance {
            Some(source) => source.search_tool_guidance(goal).await,
            None => Vec::new(),
        }
    }

    /// If the top guidance hit clears `guidance_match_floor` and names at least one tool, skip
    /// the classify LLM call entirely and route straight to `ExecuteDirect` with `relevant_mcps`
    /// set from the hit — `seed_calls` stays empty (the executor decides every step, exactly as
    /// `DEFAULT_SYSTEM_PROMPT` documents for a classifier-produced `ExecuteDirect`). This can only
    /// ever *hint* which MCPs are relevant: the guard pipeline still runs unconditionally
    /// afterward (capability/consequence/zone/depth/confidence), and `relevant_mcps` is itself
    /// only ever a narrowing *within* what's already granted (`build_turn_runtime` intersects it
    /// with the real grant) — never a way to bypass either. This is the same safety property the
    /// removed verb-keyword advisor violated (silently dropping tools for verb-less phrasing);
    /// unlike that mechanism, a low-confidence or toolless hit here falls through to full
    /// classification against the untouched catalog, not a narrowed one.
    fn guidance_short_circuit(&self, hits: &[GuidanceHit]) -> Option<DispatchDecision> {
        let top = hits.first()?;
        if top.score < self.tuning.guidance_match_floor || top.tools_used.is_empty() {
            return None;
        }
        tracing::info!(
            score = top.score,
            tools = ?top.tools_used,
            "classification short-circuited by procedural memory guidance"
        );
        Some(DispatchDecision {
            action: DispatchAction::ExecuteDirect {
                seed_calls: Vec::new(),
                relevant_mcps: top.tools_used.clone(),
                delivery: Delivery::Summarize,
            },
            confidence: top.score,
            rationale: format!("procedural memory guidance: {}", top.content),
        })
    }

    /// RECORD (`liberado-dispatch-logic-spec.md` §2 step 5): save a new guidance directive once a
    /// dispatch's outcome is known. Only `ExecuteDirect`/`DispatchSubagent` decisions name
    /// concrete tools worth remembering; `Clarify`/`Propose` carry nothing to record. Best-effort
    /// and a no-op without a configured guidance source — callers invoke this after execution
    /// resolves into a `Report`, once they know whether the decision actually worked.
    pub async fn record_outcome(&self, goal: &str, decision: &DispatchDecision) {
        let Some(source) = &self.guidance else {
            return;
        };
        let (task_type, tools_used) = match &decision.action {
            DispatchAction::ExecuteDirect {
                relevant_mcps,
                seed_calls,
                ..
            } if !relevant_mcps.is_empty() || !seed_calls.is_empty() => {
                let tools = if !relevant_mcps.is_empty() {
                    relevant_mcps.clone()
                } else {
                    let mut names: Vec<String> = seed_calls
                        .iter()
                        .map(|c| mcp_of(&c.tool).to_string())
                        .collect();
                    names.sort();
                    names.dedup();
                    names
                };
                (None, tools)
            }
            DispatchAction::DispatchSubagent { allowed_mcps, .. } if !allowed_mcps.is_empty() => {
                (None, allowed_mcps.clone())
            }
            _ => return,
        };
        let directive = format!("For tasks like \"{goal}\", use: {}", tools_used.join(", "));
        source
            .save_tool_guidance(&directive, task_type, tools_used)
            .await;
    }
}

/// The default classification system prompt. `pub` so external crates (the heuristics tuner) can
/// diff a candidate prompt against the real baseline.
pub const DEFAULT_SYSTEM_PROMPT: &str = "\
You are Liberado's dispatcher: a fast, careful router. Given a goal and a catalog of available \
MCPs (tools), choose exactly ONE action and return it as a single JSON object.

Actions:
- ExecuteDirect: handle a simple, low-consequence goal yourself in a short adaptive tool loop. \
`seed_calls` is just your opening move (the calls you already know you want); leave it empty to \
let the executor decide every step. Use this only when a few steps clearly suffice.
- DispatchSubagent: hand a complex, multi-step, or open-ended goal to a narrowly-scoped subagent.
- Clarify: ask the main agent to resolve ambiguity or a missing parameter before acting.

Bias to safety: when uncertain, or when consequences are high, prefer Clarify or DispatchSubagent \
over ExecuteDirect. Set `confidence` honestly in [0,1].

Three specific cases worth getting right:
- Building or modifying an MCP tool only ever produces a draft PR for human review — that's \
reversible and low-consequence, so route it as ExecuteDirect confidently, but only when a \
code-dispatch (or equivalent tool-building) MCP actually appears in the catalog you were given. A \
goal that talks about building a tool while no such MCP is in your catalog is a capability/tooling \
gap, not something to route around — Clarify instead of assuming one exists. Don't Clarify or \
DispatchSubagent for a real code-dispatch goal unless the goal itself is ambiguous (e.g. which \
existing tool to modify is unclear).
- Simple vault/task lookups and filters (list tasks, filter by tag/area, read one note, search \
for a known title) should be ExecuteDirect with relevant_mcps naming the vault MCP from the \
catalog (e.g. turbovault) — not DispatchSubagent. Prefer a seed_calls opening move when the \
tool is obvious (list_tasks, search, read_note).
- Open-ended analysis across multiple notes or a range of entries (summarizing, finding recurring \
themes, multi-step synthesis) is complex enough to warrant a DispatchSubagent with its own \
context slice, even when no single step is individually hard.

Return ONLY JSON of the form (these fields, plus the optional `delivery` described below — never \
invent others):
{\"action\":{\"ExecuteDirect\":{\"seed_calls\":[{\"tool\":\"mcp:tool\",\"args\":{}}],\"relevant_mcps\":[\"...\"]}},\"confidence\":0.9,\"rationale\":\"...\"}
{\"action\":{\"DispatchSubagent\":{\"goal\":\"...\",\"allowed_mcps\":[\"...\"],\"success_criteria\":[\"...\"]}},\"confidence\":0.8,\"rationale\":\"...\"}
{\"action\":{\"Clarify\":{\"questions\":[\"...\"],\"what_blocked\":\"ambiguous\"}},\"confidence\":0.4,\"rationale\":\"...\"}

For ExecuteDirect, also set `relevant_mcps` to the names (from the catalog) of the MCPs this goal \
actually needs — same idea as DispatchSubagent's `allowed_mcps`, just for the direct-execution \
case. Leave it empty if the goal doesn't clearly need any MCP, or if you're unsure (it only \
narrows what the executor sees; it is never the sole source of truth for what's allowed).

For DispatchSubagent, emit goal, allowed_mcps (names from the catalog), and success_criteria. \
`allowed_mcps` is the subagent's tool scope: list every catalog MCP it will need (vault/note/task \
goals need the vault MCP name as it appears in the catalog, e.g. turbovault). Prefer a tight list; \
empty allowed_mcps means the full dispatcher grant (broad — avoid unless truly necessary). Do not \
invent MCP names or capability objects. `seed_calls` may be omitted (empty).

Either action may also set `delivery` to say where the finished result should GO. Omit it \
(the default) and the result comes back to the main agent, which tells the human about it in its \
own words — right for anything the human will want to discuss, and for anything that ACTS on the \
world. Set \
{\"Vault\":{\"path\":\"<zone>/<name>.md\"}} instead when the result is a document the human asked \
to have written down — research write-ups, deep-dive summaries, reports they said to save. The \
system then files the report at that path verbatim and tells the human where it is. This applies \
just as much to a simple `ExecuteDirect` as to a subagent: \"read today's notes and save me a \
summary to research/\" is a direct execution with `delivery` set, and the direct executor is told \
its report IS the saved document.

Set `depth` to \"deep\" for open-ended gathering — deep research, multi-source synthesis, review \
across many notes — which needs far more turns than the default. Leave it unset otherwise. Depth is \
about how much work the goal is, never about which MCPs it uses.

The SYSTEM performs that write, not the executor or subagent — so `delivery` does not need, and is not helped \
by, a writing tool in `allowed_mcps`. Scope `allowed_mcps` purely by what the subagent or executor must READ \
to do the work, exactly as you would without `delivery`: a goal that reads notes or tasks still \
lists the vault MCP, because reading is what it is there for. Just don't add an MCP whose only \
purpose would be saving the report — there is nothing for it to do. The path must start with one \
of the vault zones listed below the MCP catalog, spelled exactly (they are case-sensitive; a zone \
not on that list is refused). When in doubt, omit `delivery`.";

/// Loose schema for v1 — the prompt carries the shape. A precise JSON Schema (e.g. via `schemars`)
/// is a follow-up that improves real-provider reliability.
/// The JSON Schema for a classifier reply — the shape a `DispatchDecision` must arrive in.
///
/// # Why this exists now
///
/// It used to be the placeholder `{"type": "object"}`, which describes nothing, and the provider
/// discarded even that in favour of `{"type":"json_object"}` — "valid JSON, any shape". So the
/// classifier's output shape rested entirely on prompt text, at temperature 0 with reasoning off, on
/// a small fast router model. On 2026-07-28 both morning crons died on replies that would not decode.
///
/// With a real schema the **backend** constrains decoding: a non-conforming token cannot be emitted.
///
/// # Written for `strict` mode
///
/// Every object sets `additionalProperties: false` and lists every property in `required`. That is
/// what OpenAI-compatible strict mode obliges, and it is why fields that are `#[serde(default)]` in
/// Rust are *required* here — the model must emit `"seed_calls": []` rather than omit it. Serde still
/// accepts the omission, so a backend that ignores the schema is no worse off than before.
///
/// # Only what the classifier may produce
///
/// Three variants, not four. `Propose` is a **post-guard downgrade**, never a classification — the
/// guards route *into* it. Leaving it out means the model cannot emit it even by accident, which is a
/// constraint the prompt could only ask for politely.
///
/// The variants are externally tagged (`{"ExecuteDirect": {…}}`) because that is serde's default
/// representation for this enum, and the schema has to match what `serde_json` will actually accept.
/// If the enum ever gains `#[serde(tag = "…")]`, this must change with it — the round-trip test below
/// is what will tell you.
fn decision_schema() -> serde_json::Value {
    let variant = |name: &str, payload: serde_json::Value| {
        serde_json::json!({
            "type": "object",
            "properties": { name: payload },
            "required": [name],
            "additionalProperties": false,
        })
    };

    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "description": "Exactly one of the three classifications.",
                "anyOf": [
                    // `seed_calls` is deliberately absent, and this is the one real cost of strict
                    // mode: a seed call carries a free-form `args` object, and strict mode cannot
                    // express "an object of arbitrary shape" — it requires `additionalProperties:
                    // false`, which permits only the properties you list, i.e. none.
                    //
                    // So the choice was the opening move or the hard guarantee. The guarantee wins:
                    // `seed_calls` is an optimisation whose absence is already well-defined ("let the
                    // executor decide every step"), and the executor's loop is adaptive by design,
                    // whereas a router that cannot be trusted to emit parseable output fails the
                    // whole dispatch. Serde still accepts the field, so nothing else changes if it
                    // ever comes back.
                    variant("ExecuteDirect", serde_json::json!({
                        "type": "object",
                        "properties": {
                            "relevant_mcps": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description":
                                    "MCP names relevant to this goal. Empty means no narrowing.",
                            },
                        },
                        // Deliberately a subset of the Rust type's fields: `seed_calls` and
                        // `delivery` are omitted for the same reason DispatchSubagent omits them —
                        // `delivery` is optional (defaults to `Summarize`) and the prompt already
                        // describes it; strict mode cannot express an optional property, so listing
                        // it would force the model to emit it every time. Serde still accepts the
                        // field, so a backend that ignores the schema is no worse off.
                        "required": ["relevant_mcps"],
                        "additionalProperties": false,
                    })),
                    variant("DispatchSubagent", serde_json::json!({
                        "type": "object",
                        "properties": {
                            "goal": {
                                "type": "string",
                                "description": "Restated, self-contained goal for the subagent.",
                            },
                            "allowed_mcps": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "MCPs the subagent may see. Empty = all in scope.",
                            },
                            "success_criteria": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "How the subagent knows it is done.",
                            },
                        },
                        // Deliberately a subset of the Rust type's fields. `capabilities`,
                        // `delivery`, `depth`, `model` and `correlation_id` are ours to decide, not
                        // the model's — `correlation_id` especially is an internal id the dispatcher
                        // mints. Omitting them from the schema means the model cannot set them, and
                        // serde's `#[serde(default)]` fills them in.
                        "required": ["goal", "allowed_mcps", "success_criteria"],
                        "additionalProperties": false,
                    })),
                    variant("Clarify", serde_json::json!({
                        "type": "object",
                        "properties": {
                            "questions": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "What the main agent must resolve first.",
                            },
                            "what_blocked": {
                                "type": "string",
                                // snake_case: `BlockReason` is `#[serde(rename_all = "snake_case")]`,
                                // so PascalCase here would have made every Clarify fail to
                                // deserialize — reintroducing the exact failure being fixed, from
                                // the other side. The test below checks each against the real type.
                                "enum": ["ambiguous", "missing_param", "capability_gap"],
                                "description": "Why this could not be classified into an action.",
                            },
                        },
                        "required": ["questions", "what_blocked"],
                        "additionalProperties": false,
                    })),
                ],
            },
            "confidence": {
                "type": "number",
                "description": "0.0-1.0 confidence in this classification.",
            },
            "rationale": {
                "type": "string",
                "description": "One line explaining the choice. Not shown to the user.",
            },
        },
        "required": ["action", "confidence", "rationale"],
        "additionalProperties": false,
    })
}

/// The safe default when classification can't be trusted: ask the main agent.
fn clarify_fallback() -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec![clarify_question(BlockReason::UnusableOutput)],
            // NOT LowConfidence. The model did not answer in the required shape at all, which is a
            // transient provider problem; low confidence means it understood and is unsure, which
            // is a goal problem. Sharing one reason made a failed cron indistinguishable from an
            // ambiguous one, and telling them apart meant re-running the classifier offline.
            //
            // Note this fallback is NOT a bypass of the guard pipeline: `classify` returns it and
            // `dispatch` runs `guards::evaluate` over it like any other decision — so an unattended
            // actor's decode failure is caught by the AskHuman guard rather than delivered as a
            // question to nobody.
            what_blocked: BlockReason::UnusableOutput,
        },
        confidence: 0.0,
        rationale: "classification output was unusable".into(),
    }
}

/// Resolve a guard violation to a downgraded decision. A high-consequence or zone-restricted block
/// on a *concrete* `ExecuteDirect` (a non-empty seed call list — a known action like "send this
/// email") or on a `DispatchSubagent` (which always carries a restated goal — the classifier's one
/// required field, so there is always something concrete enough to propose) becomes a `Propose`,
/// carrying the action for human approval (Decision 11). Every other case stays a `Clarify` — the
/// most conservative output when there is nothing concrete to propose. `ZoneRestricted` (§6 #2)
/// gets the exact same treatment as `HighConsequence` here — both are "a human needs to approve
/// this specific action before it runs," just gated on a different axis (target zone vs. general
/// riskiness) — there's no reason for one to produce a `Propose` and the other only a `Clarify`.
///
/// TODO: the one still-deferred fuzzy case is an empty-seed `ExecuteDirect` (including a bare
/// magnitude-gate hit with no seed calls) — there is no fixed action to propose, since
/// `ExecuteDirect` carries no goal of its own (unlike `DispatchSubagent`); the caller's `goal`
/// isn't threaded into this function today. Approving it would mean "run the adaptive loop on this
/// goal," which needs the same runtime-gated-execution shape `Orchestrator::execute_approved`'s
/// `Subagent` arm now has — likely a `ProposedAction::AdaptiveGoal { goal, relevant_mcps }` run via
/// `Task::new(DIRECT_INSTRUCTIONS, goal)` under a gated runtime, mirroring `Orchestrator::run`'s
/// `ExecuteDirect` arm. Left for a follow-up rather than folded in here.
fn downgrade(classified: DispatchDecision, reason: BlockReason) -> DispatchDecision {
    let confidence = classified.confidence;
    let rationale = classified.rationale;

    // An unattended block keeps whatever the classifier actually wanted to ask, with the
    // explanation appended rather than substituted.
    //
    // The question is the diagnosis: "which project did you mean?" tells you the cron goal is
    // under-specified and roughly how, while a bare "this runs unattended" tells you only that
    // something failed. Nobody will answer it, but somebody will *read* it — hours later, out of
    // context, in a cron result — and that reader needs the specifics. Replacing them was a real
    // regression, caught by `a_reaction_that_needed_a_human_fails_honestly_rather_than_reporting_success`.
    if reason == BlockReason::Unattended
        && let DispatchAction::Clarify { questions, .. } = classified.action
    {
        let mut questions = questions;
        questions.push(clarify_question(BlockReason::Unattended));
        return DispatchDecision {
            action: DispatchAction::Clarify {
                questions,
                what_blocked: BlockReason::Unattended,
            },
            confidence,
            rationale: format!("guard downgrade: {reason:?}"),
        };
    }
    if matches!(
        reason,
        BlockReason::HighConsequence | BlockReason::ZoneRestricted
    ) {
        match classified.action {
            DispatchAction::ExecuteDirect { seed_calls, .. } if !seed_calls.is_empty() => {
                return downgrade_to_propose_tool_calls(seed_calls, confidence, rationale);
            }
            DispatchAction::DispatchSubagent {
                goal,
                capabilities,
                allowed_mcps,
                success_criteria,
                ..
            } => {
                return downgrade_to_propose_subagent(
                    goal,
                    capabilities,
                    allowed_mcps,
                    success_criteria,
                    confidence,
                    rationale,
                );
            }
            _ => {}
        }
    }
    downgrade_to_clarify(confidence, reason)
}

/// Build the `Propose` a high-consequence concrete `ExecuteDirect` downgrades to, preserving the
/// original decision's seed calls (as the proposed action), confidence, and rationale. Takes the
/// exact payload rather than the whole `DispatchDecision` — the caller's match arm is what proves
/// this is a concrete `ExecuteDirect`, so there is nothing left to assert (or panic on) in here.
fn downgrade_to_propose_tool_calls(
    seed_calls: Vec<ToolCall>,
    confidence: f32,
    rationale: String,
) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: ProposedAction::ToolCalls(seed_calls),
            rationale: rationale.clone(),
        },
        confidence,
        rationale,
    }
}

/// Build the `Propose` a high-consequence `DispatchSubagent` downgrades to, preserving the goal,
/// narrowed capabilities, MCP scoping, and success criteria the classifier chose —
/// `Orchestrator::execute_approved` dispatches the subagent exactly as scoped here once a human
/// approves it. Takes the exact payload rather than the whole `DispatchDecision`, same reasoning as
/// `downgrade_to_propose_tool_calls` above.
fn downgrade_to_propose_subagent(
    goal: String,
    capabilities: CapabilitySet,
    allowed_mcps: Vec<String>,
    success_criteria: Vec<String>,
    confidence: f32,
    rationale: String,
) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Propose {
            proposed_action: ProposedAction::Subagent {
                goal,
                capabilities,
                allowed_mcps,
                success_criteria,
            },
            rationale: rationale.clone(),
        },
        confidence,
        rationale,
    }
}

/// Build the conservative `Clarify` a guard downgrade resolves to, preserving the model's
/// confidence for the trace.
fn downgrade_to_clarify(confidence: f32, reason: BlockReason) -> DispatchDecision {
    DispatchDecision {
        action: DispatchAction::Clarify {
            questions: vec![clarify_question(reason)],
            what_blocked: reason,
        },
        confidence,
        rationale: format!("guard downgrade: {reason:?}"),
    }
}

fn clarify_question(reason: BlockReason) -> String {
    match reason {
        BlockReason::CapabilityGap => {
            "This needs a capability I wasn't granted — should I be given access, or handle it differently?".into()
        }
        BlockReason::HighConsequence => {
            "This is far-reaching or hard to undo (a sweeping change, or something that leaves the system like an email) — should I go ahead?".into()
        }
        BlockReason::ZoneRestricted => {
            "This would write somewhere that needs your review before it happens — should I go ahead?".into()
        }
        BlockReason::DepthLimit => {
            "This reaction chain has gone deep enough that I'm pausing it — should it continue?".into()
        }
        BlockReason::LowConfidence => {
            "I'm not confident enough to act on this — can you confirm the intent?".into()
        }
        // The two below are the unattended cases, and they are deliberately NOT questions. Nobody
        // is going to answer them: they are read later, out of context, in a cron result or a
        // session summary. So each states what failed and what to change, because the only useful
        // thing a dead-end disposition can do is tell you where to go fix it.
        BlockReason::UnusableOutput => {
            "The router returned output that could not be read as a decision (malformed or empty). \
             This is usually transient; if it repeats for the same goal, the goal or the router \
             model needs attention."
                .into()
        }
        BlockReason::Unattended => {
            "This goal could not be routed, and this dispatch runs unattended — there is no one to \
             ask, so nothing was done. Fix the goal so it routes without clarification, or grant \
             the AskHuman capability if a person should be consulted."
                .into()
        }
        BlockReason::Ambiguous => "This goal is ambiguous — which interpretation did you mean?".into(),
        BlockReason::MissingParam => {
            "A required detail is missing — can you provide it?".into()
        }
    }
}

/// A compact, privacy-preserving correlation key for the goal in traces. `DefaultHasher`'s output
/// is not stable across Rust versions — fine for within-run correlation, not cross-version.
fn goal_hash(goal: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    goal.hash(&mut hasher);
    hasher.finish()
}

/// Mint a correlation id for a `DispatchSubagent` whose id the model omitted (it now defaults to
/// empty so terse replies still decode). Derived from the goal hash so retries of the same goal
/// share an id (idempotency + loop-breaking); never the model's to invent.
fn ensure_correlation(decision: &mut DispatchDecision, goal_hash: u64) {
    if let DispatchAction::DispatchSubagent { correlation_id, .. } = &mut decision.action
        && correlation_id.is_empty()
    {
        *correlation_id = format!("sub:{goal_hash:x}");
    }
}

/// Log the classifier (or guidance short-circuit) decision with MCP/tool fields for dogfood.
fn log_classified_decision(decision: &DispatchDecision, model: &str) {
    match &decision.action {
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            ..
        } => log_execute_direct(model, decision, seed_calls, relevant_mcps),
        DispatchAction::DispatchSubagent {
            allowed_mcps, goal, ..
        } => log_dispatch_subagent(model, decision, allowed_mcps, goal),
        DispatchAction::Clarify { what_blocked, .. } => log_clarify(model, decision, what_blocked),
        DispatchAction::Propose { .. } => log_propose(model, decision),
    }
}

fn log_execute_direct(
    model: &str,
    decision: &DispatchDecision,
    seed_calls: &[ToolCall],
    relevant_mcps: &[String],
) {
    let seeds: Vec<&str> = seed_calls.iter().map(|c| c.tool.as_str()).collect();
    tracing::info!(
        %model,
        action = "ExecuteDirect",
        confidence = decision.confidence,
        relevant_mcps = ?relevant_mcps,
        seed_tools = ?seeds,
        rationale = %decision.rationale,
        "classified decision (pre-guard)"
    );
}

fn log_dispatch_subagent(
    model: &str,
    decision: &DispatchDecision,
    allowed_mcps: &[String],
    goal: &str,
) {
    tracing::info!(
        %model,
        action = "DispatchSubagent",
        confidence = decision.confidence,
        allowed_mcps = ?allowed_mcps,
        subgoal = %goal.chars().take(120).collect::<String>(),
        rationale = %decision.rationale,
        "classified decision (pre-guard)"
    );
}

fn log_clarify(model: &str, decision: &DispatchDecision, what_blocked: &BlockReason) {
    tracing::info!(
        %model,
        action = "Clarify",
        confidence = decision.confidence,
        ?what_blocked,
        "classified decision (pre-guard)"
    );
}

fn log_propose(model: &str, decision: &DispatchDecision) {
    tracing::info!(
        %model,
        action = "Propose",
        confidence = decision.confidence,
        "classified decision (pre-guard)"
    );
}

/// Map classifier MCP strings to catalog MCP names and drop unknowns.
///
/// The model often emits tool-shaped names (`turbovault:list_tasks`) or bare tools (`list_tasks`)
/// in `relevant_mcps` / `allowed_mcps`, or bare seeds. Those fail `grants_mcp` (grants are MCP
/// names only). Empty lists after sanitize mean "no further narrowing" (full grant ceiling).
pub(crate) fn sanitize_decision_mcps(decision: &mut DispatchDecision, catalog: &[McpDescriptor]) {
    // Empty catalog = tests / misconfigured host with no MCP list to validate against — leave
    // the decision alone rather than stripping every name.
    if catalog.is_empty() {
        return;
    }
    let known: std::collections::HashSet<&str> = catalog.iter().map(|m| m.name.as_str()).collect();

    match &mut decision.action {
        DispatchAction::ExecuteDirect {
            seed_calls,
            relevant_mcps,
            ..
        } => {
            *relevant_mcps =
                normalize_mcp_list(std::mem::take(relevant_mcps), &known, "relevant_mcps");
            seed_calls.retain(|c| {
                let mcp = mcp_of(&c.tool);
                if known.contains(mcp) {
                    true
                } else {
                    tracing::warn!(
                        tool = %c.tool,
                        mcp,
                        "dropping seed_call whose MCP is not in the catalog"
                    );
                    false
                }
            });
        }
        DispatchAction::DispatchSubagent { allowed_mcps, .. } => {
            *allowed_mcps =
                normalize_mcp_list(std::mem::take(allowed_mcps), &known, "allowed_mcps");
        }
        DispatchAction::Clarify { .. } | DispatchAction::Propose { .. } => {}
    }
}

fn normalize_mcp_list(
    raw: Vec<String>,
    known: &std::collections::HashSet<&str>,
    field: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    for entry in raw {
        // Accept either catalog MCP names or `mcp:tool` forms.
        let mcp = mcp_of(&entry).to_string();
        if known.contains(mcp.as_str()) {
            if !out.iter().any(|x| x == &mcp) {
                if mcp != entry {
                    tracing::debug!(
                        field,
                        raw = %entry,
                        %mcp,
                        "normalized MCP reference to catalog name"
                    );
                }
                out.push(mcp);
            }
        } else {
            tracing::warn!(
                field,
                raw = %entry,
                mcp = %mcp,
                "dropping unknown MCP reference (not in catalog)"
            );
        }
    }
    out
}

/// Deterministic post-classification enforcement of `DispatchTuning::narrow_direct_tools`: when
/// off, clear whatever `relevant_mcps` the classifier produced so every downstream consumer
/// (`Orchestrator`, `ChatSessions`) sees the same "no narrowing" signal it would if the model had
/// never populated the field — one simple rule everywhere, no separate tunable-awareness needed
/// per consumer. Same pattern as `ensure_correlation`: deterministic code, not model-trusted.
fn enforce_narrow_direct_tools(decision: &mut DispatchDecision, narrow_direct_tools: bool) {
    if !narrow_direct_tools
        && let DispatchAction::ExecuteDirect { relevant_mcps, .. } = &mut decision.action
    {
        relevant_mcps.clear();
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_tests_more.rs"]
mod tests_more;
