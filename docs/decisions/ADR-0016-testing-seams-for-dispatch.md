---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0016
open_items: false
---

# ADR-0016: Testing Seams for Nondeterministic Dispatch

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0016 (`testing-seams-for-dispatch`)

## Context

Recorded as Decision 16 in the historical architecture decision log.

## Decision

**Integration tests injected at the two ingress points** — a simulated **user prompt** or a simulated **vault event** — run through the live pipeline with externals mocked (mock provider behind the provider trait, mock MCP servers, a real temp vault, injected clock + correlation-ID source), asserting on observable outcomes (vault writes, proposals, the `Report`, which tool calls fired or were suppressed). The key enabler: safety lives in **deterministic guards that run *after* the model**, so most of the system is exactly assertable and only classification *quality* (never safety) is probabilistic. Two verification methods inside scenarios: **mock-provider replay** (deterministic CI regression) and a **real-model eval suite** reporting routing accuracy + safe-default rate + a **safety-regression metric that must never increase**. **Logging is the fixture pipeline**: the Decision 12 trace ? `record` mode ? golden scenario ? permanent regression test.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-testing-and-eval-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

Create a mocked-provider / recorded-fixture harness early so liberado's classification and dispatch logic can be tested deterministically.

**Status**: Complete (specified in `liberado-testing-and-eval-spec.md`).

Decision 16: **Integration tests injected at the two ingress points** — a simulated **user prompt** or a simulated **vault event** — run through the live pipeline with externals mocked (mock provider behind the provider trait, mock MCP servers, a real temp vault, injected clock + correlation-ID source), asserting on observable outcomes (vault writes, proposals, the `Report`, which tool calls fired or were suppressed). The key enabler: safety lives in **deterministic guards that run *after* the model**, so most of the system is exactly assertable and only classification *quality* (never safety) is probabilistic. Two verification methods inside scenarios: **mock-provider replay** (deterministic CI regression) and a **real-model eval suite** reporting routing accuracy + safe-default rate + a **safety-regression metric that must never increase**. **Logging is the fixture pipeline**: the Decision 12 trace ? `record` mode ? golden scenario ? permanent regression test.
