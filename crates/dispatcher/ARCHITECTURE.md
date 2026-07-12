# liberado-dispatcher — the safe router (decide, don't do)

Takes a goal + minimal context and produces a typed `DispatchDecision` — `ExecuteDirect`,
`DispatchSubagent`, `Clarify`, or `Propose` (Decision 1/11). It **decides**; it does not execute
(that's the [orchestrator](../orchestrator/ARCHITECTURE.md) +
[executor](../executor/ARCHITECTURE.md)).

## Pipeline

```
DispatchRequest ──► classify ──► guards.evaluate ──► downgrade() ──► DispatchDecision
   goal,            (1 structured   (deterministic,      (Clarify, or
   catalog,          inference,      downgrade-only)      Propose for a
   capabilities,     temp 0)                              concrete action)
   reaction_depth,
   zone_write_classes
```

1. **classify** (`lib.rs`) — one structured-output inference at temperature 0 turns the goal + MCP
   catalog into a candidate decision via `complete_json`. A decode/empty failure falls back to a
   safe `Clarify`, never an action. High-confidence **procedural-memory guidance** may short-circuit
   to `ExecuteDirect` with `relevant_mcps` from the hit (still fully guarded afterward).
2. **sanitize MCP names** — after classify, rewrite tool-shaped names (`turbovault:list_tasks` →
   `turbovault`) and drop bare unknowns (`list_tasks`) that aren't catalog MCP names. Empty
   `relevant_mcps` / `allowed_mcps` after sanitize means "no further narrowing" (full grant ceiling),
   not CapabilityGap. Prevents false gaps when the model confuses tools with MCP servers.
3. **guards** (`guards.rs`) — deterministic checks that run **after** the model and can only
   *downgrade* autonomy. Priority: capability gap → consequence gate → zone-write-class gate →
   magnitude gate → reaction-depth limit → confidence floor. A capability gap, depth limit, or
   low-confidence hit downgrades to `Clarify`; a consequence or zone-write-class hit on a
   *concrete* action (a non-empty `ExecuteDirect` or any `DispatchSubagent`) downgrades to
   `Propose` instead (Decision 11) — both are "needs human approval before running," just gated on
   different axes (general riskiness vs. the specific target zone). Guards never upgrade or invent
   an action. Capability-gap logs include the missing MCP name.

   The zone-write-class gate is pre-flight only, checking `ExecuteDirect`'s seed calls against
   per-tool zone declarations (`McpDescriptor.default_zone`/`tool_zones`, human-authored in
   `topology.toml`'s `McpConfig`, unlabeled tools inheriting the MCP's `default_zone`) resolved via
   `liberado_common::zone_write_restriction` and checked against `DispatchRequest.zone_write_classes`
   (from `Policy.zones`). The real, always-enforced boundary for every call (including a subagent's
   later adaptive ones) is `RiskGatedToolRuntime` (`liberado-executor`), which calls the same
   `zone_write_restriction` function (not just "a shared resolution helper" informally — the two
   enforcement points literally can't drift on what counts as restricted, unified 2026-07-05) — this
   pre-flight check only ever sees the classifier's opening move.

## The core safety property

**Safety is engineered, not prompted.** The model proposes; deterministic code disposes, and only
ever toward *less* autonomy. A model that "wants to just do it" cannot escape a guard — e.g. a
0.95-confidence `ExecuteDirect` for an MCP the request wasn't granted is downgraded to
`Clarify(CapabilityGap)`. This was validated against live inference.

## Surface

- `Dispatcher` (holds `Arc<dyn Provider>`, `DispatchTuning`, max reaction depth), `dispatch()`.
- `DispatchRequest` (goal, `catalog: Vec<McpDescriptor>`, capabilities, reaction depth,
  `zone_write_classes`), `McpDescriptor` (re-exported from `liberado-common` — the catalog entry
  the model sees, now zone-aware for the zone-write-class gate).
- `SYSTEM_PROMPT` — the classifier prompt. Biasing it toward delegation (vs. "just working") is a
  known, deliberate tuning surface; the structural backstop is that policy/guards can *force* a
  route regardless of the model's lean.

## Dependencies

- Depends on: `liberado-common` (decision/capability/zone-resolution types), `liberado-config-loader`
  (`DispatchTuning`), `liberado-provider` (inference).
- Depended on by: `daemon`, `cli`, and (consumes its output) `orchestrator`.

## Tests

`lib.rs` + `guards.rs` inline tests: temp-0 JSON classification, safe fallback, each guard downgrade
(capability, consequence, zone-write-class, magnitude, depth, confidence) with their precedence,
and the Clarify-vs-Propose split for consequence/zone-write-class hits on a concrete action.
