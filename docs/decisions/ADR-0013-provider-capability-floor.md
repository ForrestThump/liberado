---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0013
open_items: false
---

# ADR-0013: Provider Capability Floor / Minimum Contract

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0013 (`provider-capability-floor`)

## Context

Recorded as Decision 13 in the historical architecture decision log.

## Decision

**Role-tiered, not a single floor.**
- **Hard floor (every role)**: native **tool-calling** OR a reliable **JSON mode**. Text-only models are out of scope for v1 (constrained-decoding shim is the deferred escape hatch).
- **Control plane (main agent + dispatcher)** — the capable models. The **dispatcher's hard requirement is reliable structured output** (the typed `DispatchDecision`); the main agent needs solid tool-calling + instruction-following + conversational quality.
- **Work plane (subagents)** — floor is tool-calling; the **dispatcher picks the model per-dispatch by task complexity** (cheap ~8B for easy tasks, larger for hard ones — `DispatchDecision` already carries `model: Option<ModelChoice>`). This is where cheap models earn their keep.
- **Mechanism (feeds the config validator, Decision 14)**: a `ModelProfile` declares each model's capabilities (`tool_calling`, `structured_output`, `context_window`, tier/cost). Config assigns models?roles; the loader **fail-fast rejects** any model that doesn't meet its role's required caps (this is what keeps dispatch from breaking on a model swap). Optional startup **canary smoke-test** verifies tool-calling/JSON actually work.
- **R…

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- See crate Rustdoc and tests for the current implementation of this decision.

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
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
