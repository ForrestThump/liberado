---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0013
open_items: false
---

# ADR-0013: Provider Capability Floor / Minimum Contract

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0013 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Provider capability is role-tiered, not a single floor: hard floor is native tool-calling or reliable JSON mode; control plane (main + dispatcher) needs strong structured output / tool-calling; work plane models are chosen per task. ModelProfile + config validation fail-fast rejects unfit role assignments; runtime degrades malformed structured output toward Clarify, never crash.

## Consequences

Model swaps cannot silently break dispatch. Cheap models remain usable for subagents. Dispatcher runs at temperature 0.

## Rejected alternatives

A single global minimum model for all roles. Text-only models as first-class v1 providers without tool/JSON capability.

## Implementation and tests

- See crate Rustdoc and tests for the current implementation of this decision.

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

Define the minimum tool-calling, JSON mode, and structured output reliability a provider must support so liberado's dispatch protocol doesn't break when switching models.

**Status**: Complete

Decision 13: **Role-tiered, not a single floor.**
- **Hard floor (every role)**: native **tool-calling** OR a reliable **JSON mode**. Text-only models are out of scope for v1 (constrained-decoding shim is the deferred escape hatch).
- **Control plane (main agent + dispatcher)** — the capable models. The **dispatcher's hard requirement is reliable structured output** (the typed `DispatchDecision`); the main agent needs solid tool-calling + instruction-following + conversational quality.
- **Work plane (subagents)** — floor is tool-calling; the **dispatcher picks the model per-dispatch by task complexity** (cheap ~8B for easy tasks, larger for hard ones — `DispatchDecision` already carries `model: Option<ModelChoice>`). This is where cheap models earn their keep.
- **Mechanism (feeds the config validator, Decision 14)**: a `ModelProfile` declares each model's capabilities (`tool_calling`, `structured_output`, `context_window`, tier/cost). Config assigns models?roles; the loader **fail-fast rejects** any model that doesn't meet its role's required caps (this is what keeps dispatch from breaking on a model swap). Optional startup **canary smoke-test** verifies tool-calling/JSON actually work.
- **Runtime resilience**: malformed structured output ? treated like low confidence ? bounded retry/repair (re-prompt with schema) ? escalate to a stricter model or `Clarify`; never crash. Dispatcher runs at **temperature 0**. DeepSeek (starting provider) meets the control-plane bar.
