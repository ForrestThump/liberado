# Liberado Testing & Eval Spec — Integration Tests at the Two Ingress Points

**Status**: Resolves Tier-3 Decision 16 (testing seams for nondeterministic dispatch). Actionable.
**Owner**: Shiloh Mangus
**Last Updated**: June 21, 2026
**Related**:
- `liberado-dispatch-logic-spec.md` (§6 guards, §9 eval harness, decision/report types)
- `liberado-architecture-decisions.md` (Decision 12 tracing; Decision 13 provider contract)
- `liberado-config-spec.md` (config validation is part of the deterministic surface)

---

## 1. The Shape: Integration Tests from the Two Ingress Points

The system has exactly **two real ingress points**, so the tests are fundamentally **integration
tests**: inject an input at one of them, run the live pipeline with externals mocked, and observe
what happens.

1. **Simulated user prompt** → main agent → ContextPolicy → dispatcher → (execute | subagent |
   clarify) → outcome.
2. **Simulated vault event** → daemon subscription → attribution/de-loop → hook → dispatcher →
   outcome. (Covers inbox capture, ambient sweep, decisions/reviews reactions, etc.)

**Mocked externals** (so a scenario is hermetic and side-effect-free):
- **Provider** (inference) — a mock behind the provider trait returns canned/recorded outputs.
- **MCP servers** — in-process mocks; tool calls are recorded, not really executed.
- **Vault** — a real temp directory (Turbovault operates on it for real), seeded per scenario.
- **Clock + correlation-ID source** — injected/fixed so windows and journal markers are deterministic.

**Observable outcomes a scenario asserts on**: vault writes (paths + content/frontmatter),
proposals created, the `Report` returned to the main agent, which MCP tool calls were issued (and
with what narrowed capabilities), and which were *suppressed* by a guard or loop-breaking.

A scenario reads like: *"seed vault with goal X; inject prompt 'review my recent decisions'; assert a
subagent was dispatched with `decisions` read capability, a `reviews/…` artifact was written, and the
Report summarized it."* Or: *"drop a note in `inbox/` with `#ready-now`; advance the clock past the
settle window; assert a task was created and the note moved to `processed/` with a breadcrumb."*

## 2. The Core Insight That Makes This Tractable

The architecture deliberately pushed load-bearing correctness into **deterministic guards** (dispatch
spec §6: capability / zone-write-class / consequence / reaction-depth / confidence). So within an
integration scenario, **safety is enforced by code that runs *after* the model** — meaning the parts
that must never break are deterministic and exactly assertable, and only classification *quality*
(never safety) rides on a probabilistic model. The two methods below verify the two kinds of parts a
scenario exercises.

## 3. Method A — Deterministic Assertions (the bulk)

Everything except the classifier's judgment is pure code, asserted exactly within scenarios and in
focused unit tests:

- **Guard pipeline**: given any `DispatchDecision`, assert the downgrade outcome (Execute→Subagent→
  Clarify, proposal forcing, depth halt, confidence floor). Exhaustive — this is the safety surface.
- **Capability narrowing** (intersection never widens), **loop-breaking hash join** (match / no-match /
  human-edit-after-agent), **settle/quiescence**, **idempotency journal** transitions.
- **Config validation** (`liberado config check` rules), **frontmatter schema validation**,
  **provenance attribution**.
- **Tool execution** against mock MCP servers — real client path, no real side effects.

## 4. Method B — Classification Quality (the only probabilistic part)

The dispatcher's classification step is the lone genuinely nondeterministic component. Two modes:

### 4.1 Mock-provider replay (deterministic regression)

- The mock provider returns canned outputs, so a full integration scenario becomes **deterministic
  end-to-end**. This is the day-to-day CI gate: "did a prompt / guard / schema change break dispatch?"
- **Fixtures**: `(scenario input + retrieved guidance + catalog) → expected outcome`, versioned in the
  repo. Assert on **what matters** — resolved `action` + guard downgrades + observable outcome — while
  letting `rationale` text vary.

### 4.2 Real-model eval suite (metrics, not pass/fail)

- Runs the **real** configured model(s) over a **labeled set of ingress scenarios** and reports:
  - **Routing accuracy** (right tier chosen?), **safe-default rate** (uncertainty → Clarify/propose?).
  - **Safety-regression metric** — rate of *unsafe* outcomes (a write/external action executed where it
    should have proposed/clarified). **This must never increase** on a prompt or model change; it is the
    hard gate, separate from raw accuracy.
- Used to **A/B system-prompt versions** and **compare models per role** (Decision 13 — e.g. an 8B
  subagent vs a larger one). Run occasionally / pre-change, not every commit (costs tokens, noisy).

## 5. Logging Is the Fixture Pipeline

Good logging is not just observability — it is how scenarios are born and misroutes are debugged:

```
runtime trace (Decision 12: goal hash, guidance ids, action, confidence, rationale,
               guard downgrades, await/detach, outcome)
   →  review a real dispatch  →  record as a golden scenario/fixture  →  regression-test forever
```

A `record` mode promotes a traced live run (prompt or vault event + everything that happened) into a
replayable scenario, reviewable before committing. When dispatch misbehaves in real use, the trace is
the first thing read and usually becomes the fixture that pins the corrected behavior.

## 6. Determinism Knobs

- Dispatcher inference at **temperature 0** (tests and production — classification wants determinism),
  seeded where supported.
- Injected **fixed clock** and **fixed correlation-ID source** so settle windows, recency windows, and
  journal markers are reproducible across runs.

## 7. v1 Scope

- Provider trait + **mock provider**; **mock MCP servers**; temp-vault scenario fixtures.
- An **integration-scenario runner** driven from both ingress points (prompt, vault event).
- Deterministic assertions for guards, narrowing, loop-breaking, config/schema validation.
- A starter **scenario/fixture suite** with mock-replay in CI; structured dispatch tracing + `record`
  mode to mint scenarios.

**Deferred**: the full labeled real-model eval suite + dashboards; automated prompt-A/B tooling.
(The safety-regression metric is defined now so it is never an afterthought, even if the suite starts small.)

## 8. Open Questions (non-blocking)

1. Scenario/fixture format & location — files under a dedicated `eval` crate (keeps real-model runs out
   of the fast unit-test path) vs `tests/fixtures/`. Lean a dedicated `eval` crate.
2. Curating the labeled set — hand-written seeds + promoted real traces. Lean both.
