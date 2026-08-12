---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0016
open_items: false
---

# ADR-0016: Testing Seams for Nondeterministic Dispatch

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0016 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Deterministic integration tests inject at user-prompt and vault-event ingress with mocked externals, asserting observable outcomes. Safety lives in post-model deterministic guards so most behavior is assertable; classification quality is separate eval. Logging/trace is the fixture pipeline (record mode to golden scenarios).

## Consequences

CI can regress dispatch safety without live models. Real-model evals track routing accuracy and a non-increasing safety-regression metric.

## Rejected alternatives

Only end-to-end live-model tests for core safety. Safety encoded solely in the model prompt.

## Implementation and tests

- `liberado-testing-and-eval-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

Create a mocked-provider / recorded-fixture harness early so liberado's classification and dispatch logic can be tested deterministically.

**Status**: Complete (specified in `liberado-testing-and-eval-spec.md`).

Decision 16: **Integration tests injected at the two ingress points** — a simulated **user prompt** or a simulated **vault event** — run through the live pipeline with externals mocked (mock provider behind the provider trait, mock MCP servers, a real temp vault, injected clock + correlation-ID source), asserting on observable outcomes (vault writes, proposals, the `Report`, which tool calls fired or were suppressed). The key enabler: safety lives in **deterministic guards that run *after* the model**, so most of the system is exactly assertable and only classification *quality* (never safety) is probabilistic. Two verification methods inside scenarios: **mock-provider replay** (deterministic CI regression) and a **real-model eval suite** reporting routing accuracy + safe-default rate + a **safety-regression metric that must never increase**. **Logging is the fixture pipeline**: the Decision 12 trace ? `record` mode ? golden scenario ? permanent regression test.
