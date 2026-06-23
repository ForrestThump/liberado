# liberado-dispatcher — the safe router (decide, don't do)

Takes a goal + minimal context and produces a typed `DispatchDecision` — `ExecuteDirect`,
`DispatchSubagent`, or `Clarify` (Decision 1). It **decides**; it does not execute (that's the
[orchestrator](../orchestrator/ARCHITECTURE.md) + [executor](../executor/ARCHITECTURE.md)).

## Pipeline

```
DispatchRequest ──► classify ──► guards.evaluate ──► DispatchDecision
   goal,            (1 structured   (deterministic,
   catalog,          inference,      downgrade-only)
   capabilities,     temp 0)
   reaction_depth
```

1. **classify** (`lib.rs`) — one structured-output inference at temperature 0 turns the goal + MCP
   catalog into a candidate decision via `complete_json`. A decode/empty failure falls back to a
   safe `Clarify`, never an action.
2. **guards** (`guards.rs`) — deterministic checks that run **after** the model and can only
   *downgrade* autonomy (to `Clarify`). Priority: capability gap → reaction-depth limit → confidence
   floor. They never upgrade or invent an action.

## The core safety property

**Safety is engineered, not prompted.** The model proposes; deterministic code disposes, and only
ever toward *less* autonomy. A model that "wants to just do it" cannot escape a guard — e.g. a
0.95-confidence `ExecuteDirect` for an MCP the request wasn't granted is downgraded to
`Clarify(CapabilityGap)`. This was validated against live inference.

## Surface

- `Dispatcher` (holds `Arc<dyn Provider>`, `DispatchTuning`, max reaction depth), `dispatch()`.
- `DispatchRequest`, `McpDescriptor` (the catalog entry the model sees).
- `SYSTEM_PROMPT` — the classifier prompt. Biasing it toward delegation (vs. "just working") is a
  known, deliberate tuning surface; the structural backstop is that policy/guards can *force* a
  route regardless of the model's lean.

## Dependencies

- Depends on: `liberado-common` (decision/capability types), `liberado-provider` (inference).
- Depended on by: `daemon`, `cli`, and (consumes its output) `orchestrator`.

## Tests

`lib.rs` + `guards.rs` inline tests: temp-0 JSON classification, safe fallback, and each guard
downgrade (capability, depth, confidence) with their precedence.
