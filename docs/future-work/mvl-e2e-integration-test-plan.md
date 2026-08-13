---
kind: plan
status: active
authority: implementation
domain: coding-harness
canonical_for: mvl-e2e-integration-test
open_items: false
last_verified: 2026-08-13
---

# Plan: Model View Log end-to-end integration tests

**Status**: Implemented, 2026-08-13. Path-based oracle, fixture-path e2e, and Liberado
producer cases are live. Production MVL append-and-flush emission lives at the
executor / provider boundary (roadmap **4b** / backlog **0.6**).

**Purpose**: Design an **end-to-end integration** suite that verifies a harness implements
[`docs/spec/reference/model-view-log.md`](../spec/reference/model-view-log.md) correctly — by
driving a producer, reading the real append-flushed JSONL on disk, and judging it with a
provider-agnostic conformance oracle that any conforming harness can feed.

> **Placement note (docs lifecycle #150):** This is an implementation plan, so it lives under
> `docs/future-work/` per [`doc-authority.md`](../spec/reference/doc-authority.md). The normative
> MVL contract remains in `docs/spec/reference/model-view-log.md`. The original co-location goal
> is satisfied by a link from that spec's Conformance section, not by storing a plan among
> normative reference docs.

---

## Problem: what fixtures already prove, and what they do not

### Landed (do not re-specify as new work)

Roadmap **4a** / PR #140 shipped:

| Artifact | Role |
|---|---|
| `docs/spec/reference/model-view-log.md` | Normative MVL contract (including eight Conformance bullets) |
| `docs/spec/reference/execution-log.md` | Companion execution stream and join rule |
| `crates/test-support/src/trace_contracts.rs` | Shipped pure readers: `parse_jsonl`, `reconstruct_turn`, `assert_seq_gap_free`, `assert_join_integrity`, `assert_attempt_brackets`, `assert_mvl_has_no_scheduler_leakage` |
| `crates/test-support/tests/mvl_conformance.rs` | Fixture suite: loads on-disk `sample.mvl.jsonl` + `sample.execution.jsonl` and drives those readers |
| `crates/test-support/fixtures/trace_contracts/*` | Hand-authored sample streams |

Those tests prove: **a reader can rebuild turn N from a well-formed MVL log alone**, and that
execution events join by `call_id` / turn without timestamp inference. They are **static /
fixture-reader** checks.

### Gap this plan closes

They do **not** prove that any production producer:

1. Emits MVL at the executor / provider boundary (roadmap **4b** / backlog **0.6** is still open).
2. Appends and flushes **per event** (crash survival of a live process).
3. Writes `content_shown` that byte-equals the string the tool layer actually handed the model.
4. Emits explicit `tools_changed` when a guard withdraws tools (not only a narrowed
   `tools_offered` on the next prompt).
5. Produces logs under a scripted multi-turn loop rather than a hand-written JSONL file.

The new suite is **producer → real files on disk → same oracle**. Fixtures remain the oracle's
unit baseline and the regression pin for reader rules; they are not replaced.

---

## Design principles

1. **Provider-agnostic.** No live LLM, no provider API key, no network call to a model host.
   Liberado runs under test use a scripted / mock completion source (existing
   `MockProvider` / `mock_provider([...])` patterns in `coder-agent` tests and
   `provider::mock`). Other harnesses use whatever scripted driver they already have.
2. **Harness-agnostic oracle.** The primary judge consumes **MVL JSONL paths** (and, for join
   checks, a paired execution-log path). It must not import Liberado session types, `CoderEvent`,
   or crate internals. Liberado is one **producer adapter**, not the only subject under test.
3. **Reuse `trace_contracts`.** New e2e cases call the shipped functions in
   `liberado_test_support::trace_contracts` (or a thin CLI/binary that wraps them). Do not
   re-implement reconstruction inside the integration tests.
4. **Stage Liberado emission.** Full Liberado green e2e requires roadmap **4b** / backlog **0.6**.
   Until then, implement oracle + external/fixture-path adapters immediately; gate or `#[ignore]`
   the Liberado producer cases with a clear `cfg` / feature / ignore reason that names 0.6.

---

## Suite architecture (two layers)

```
                    ┌─────────────────────────────────────┐
                    │  Conformance oracle (layer 1)       │
                    │  Input: path to *.mvl.jsonl         │
                    │         optional *.execution.jsonl  │
                    │  Uses: trace_contracts readers      │
                    │  Output: pass/fail per rule         │
                    └──────────────▲──────────────────────┘
                                   │ JSONL files only
          ┌────────────────────────┼────────────────────────┐
          │                        │                        │
 ┌────────┴────────┐    ┌──────────┴──────────┐   ┌────────┴────────┐
 │ Liberado        │    │ External harness    │   │ Static fixtures │
 │ producer adapter│    │ adapter (pi, Hermes,│   │ (already landed)│
 │ (mock provider) │    │  Kilo, …)           │   │                 │
 └─────────────────┘    └─────────────────────┘   └─────────────────┘
         layer 2a                 layer 2b              layer 0 (baseline)
```

### Layer 1 — pure conformance oracle (files in)

**Input contract** (portable; document in the oracle module/`README` next to the tests):

| Argument | Required | Meaning |
|---|---|---|
| `--mvl <path>` | yes | Append-flushed MVL JSONL for one run |
| `--execution <path>` | for join suite | Paired execution log, same `run` id |
| `--expected-content-shown <call_id>=<path>` | for honesty | Ground truth bytes for tool honesty |
| `--kill-after-seq <n>` | for crash | See crash scenario below |

**Outputs**: structured pass/fail for each of the eight Conformance rules (below). Prefer a
library entry point (`run_mvl_conformance(mvl_path, exec_path, opts) -> Report`) used by both
`cargo test` and a small CLI so non-Rust harnesses can invoke the same binary later.

**Implementation home (recommended):** extend `liberado_test_support::trace_contracts` with
suite-level helpers (`assert_system_prompt_once`, `assert_tool_catalog_once`,
`assert_tools_changed_covers_offered_diff`, crash-prefix parse) and add integration tests under
`crates/test-support/tests/` (or a dedicated `crates/mvl-conformance` binary crate if a
standalone CLI is needed for foreign harnesses). Keep the oracle free of `coder-agent` /
`executor` dependencies.

### Layer 2 — producer adapters (generate the files)

Each adapter's only job: run a harness with a **scripted model**, force the behaviours under
test (multi-turn tools, tool withdrawal, optional process kill), and leave MVL (+ optional
execution) JSONL on disk. Adapters never assert reconstruction rules themselves; they call
layer 1.

#### Liberado adapter (when 0.6 / 4b exists)

Sketch:

1. Temp workspace + git identity (CI-safe).
2. Wire `MockProvider` with a fixed script: e.g. turn 0 tool call → tool result → turn 1 stop;
   a second scenario that triggers a doom-loop / guard so tools are withdrawn mid-run.
3. Run the production coding / executor path that will own MVL emission (executor / provider
   boundary — **not** a post-hoc conversion from the end-of-run `CoderEvent` JSON document).
4. Assert the log path exists and is non-empty **before** calling the oracle.
5. Pass that path into layer 1.

Until emission lands: either skip with an explicit ignore message, or temporarily drive a
**test-only emitter at the same boundary** the production code will use (same append-flush API),
so the e2e wiring is real and 0.6 only swaps the emitter implementation. Prefer the production
boundary hook over a parallel fake path that never ships.

#### External harness adapter

Document a minimal plug-in contract:

```text
1. Produce MVL JSONL at $OUT/run.mvl.jsonl (v1 envelope, append-flushed).
2. Optionally produce $OUT/run.execution.jsonl with shared run / turn / call_id.
3. Invoke: cargo test -p liberado-test-support --test mvl_e2e_oracle -- --mvl $OUT/run.mvl.jsonl
   or: mvl-conformance --mvl $OUT/run.mvl.jsonl [--execution $OUT/run.execution.jsonl]
```

No Liberado runtime required. A converter from Kilo/pi native traces to MVL can sit in front of
the same oracle (roadmap 5 / backlog 0.7); converters are out of scope for the first Liberado e2e
PR but the oracle must not block them.

---

## Mapping: eight Conformance rules → concrete cases

Normative source: `model-view-log.md` § Conformance. Each case names **evidence**, **judge**,
and **fixture vs e2e**.

### 1. Reconstruction

> From the log alone, rebuild exact message list, system text, ordered tool definitions and
> sampling parameters for any turn N.

| | |
|---|---|
| **Judge** | `reconstruct_turn` for every turn present; assert system text, `tool_definitions`, `messages`, `params`, `tools_offered` match producer ground truth (or fixture expectations). |
| **Fixture today** | `mvl_fixture_reconstructs_every_turn` + unit tests in `trace_contracts`. |
| **E2E** | Liberado (or foreign) producer runs a multi-turn scripted loop; oracle reconstructs each turn; compare messages/params/tools to the mock script's known request view **or** to an independently recorded "what we sent the mock" side channel. Prefer comparing against a capture at the provider boundary (the bytes the mock received), not against a second reconstruction of the same log. |
| **Evidence** | Oracle report: per-turn reconstruction ok; optional golden request dump from the mock provider. |

### 2. Crash survival

> Kill the process mid-run; the log up to that point is valid JSONL and every line parses.

| | |
|---|---|
| **Judge** | `parse_jsonl` succeeds on the partial file; every line is a complete object; no trailing partial line required to parse (producers must flush complete lines). |
| **Fixture today** | Not covered as a live kill (static files are always complete). |
| **E2E** | Spawn producer; after `seq >= N` is durable on disk (poll or side-channel), kill the process (Windows: terminate process tree; Unix: `SIGKILL`). Re-open the file; parse all complete lines. Optional: assert last durable event is not only in an unflushed buffer (forces real append+flush). |
| **Evidence** | Partial JSONL path + parse success; note last `seq` retained. |

### 3. Ordering

> `seq` is gap-free and monotonic.

| | |
|---|---|
| **Judge** | `assert_seq_gap_free` on MVL stream (and execution stream when present). |
| **Fixture today** | Covered for samples. |
| **E2E** | Same check on producer output; add a negative unit case if a mock emitter ever skips `seq` (oracle must fail). |
| **Evidence** | Oracle seq check. |

### 4. System prompt recoverable

> Every distinct system prompt appears in full exactly once; every `prompt` carries its hash.

| | |
|---|---|
| **Judge** | Scan all `prompt` events: each carries `system.sha256`; non-null `system.text` appears once per distinct hash; every hash used is recoverable (feeds reconstruction). |
| **Fixture today** | Implicit via reconstruction of turn 0 (text present) and turn 1 (`text: null` + same hash). |
| **E2E** | Multi-turn run with stable system prompt; oracle asserts single full emission and hash continuity. Optional second run that **changes** system mid-session (if the harness supports it) to force a second full text emission under a new hash. |
| **Evidence** | Oracle system-prompt rules + reconstruction. |

### 5. Tool catalogue recoverable

> Every distinct ordered tool catalogue appears in full exactly once; every `prompt` carries its hash.

| | |
|---|---|
| **Judge** | Every `tool_catalog` body is unique per `sha256`; every `prompt.tool_catalog_sha256` resolves; ordered definitions match. |
| **Fixture today** | Implicit via reconstruction. |
| **E2E** | Producer offers a known catalogue; after guard withdrawal, catalogue hash may stay the same while `tools_offered` narrows (definitions list vs offered names — both must stay consistent with the spec). |
| **Evidence** | Oracle catalogue rules + reconstruction. |

### 6. Tool honesty

> `content_shown` byte-equals what the tool layer handed the model.

| | |
|---|---|
| **Judge** | For each `tool_result`, `content_shown` equals ground-truth bytes for that `call_id`. |
| **Fixture today** | Fixture asserts structural presence only; cannot prove honesty without a live tool layer. |
| **E2E** | Scripted tool returns a known string (include multi-byte UTF-8 and a truncation/offload case if the harness rewrites large results). Capture the exact string the tool runtime returned to the model path; compare to `content_shown`. If offload/truncation applies, `content_shown` must equal the **post-rewrite** string, and `full_content` / flags must match the spec. |
| **Evidence** | Per-`call_id` equality; failure message includes both lengths and a short prefix hex/diff. |

### 7. Withdrawal visible

> Any change to the offered tool set appears as `tools_changed`.

| | |
|---|---|
| **Judge** | Whenever consecutive prompts differ in `tools_offered` set, an intervening `tools_changed` must list the removal/addition (or an explicit full replacement policy documented by the producer). Presence of narrowed `tools_offered` alone is **not** enough. |
| **Fixture today** | Sample contains `tools_changed` + later prompt with narrowed list; no automated "diff implies event" rule yet. |
| **E2E** | Force a guard (e.g. doom-loop remove) under mock completions; assert `tools_changed` appears with `removed` containing the withdrawn tool; next `prompt.tools_offered` matches. |
| **Evidence** | Oracle `tools_changed` coverage check. |

### 8. Join integrity

> Every execution event that refers to a turn or call joins to one MVL event by id; no sequence
> or timestamp inference.

| | |
|---|---|
| **Judge** | `assert_join_integrity(mvl, execution)` (+ `assert_attempt_brackets` on execution). |
| **Fixture today** | `execution_fixture_joins_mvl_without_timestamps` + unit negatives. |
| **E2E** | Producer emits **both** streams; oracle joins. If a producer only implements MVL first, mark join cases as pending for that adapter rather than weakening the rule. |
| **Evidence** | Oracle join report. |

---

## Suggested test layout (implementation goal — not this plan's diff)

```
crates/test-support/
  src/trace_contracts.rs          # extend suite helpers (oracle core)
  tests/
    mvl_conformance.rs            # KEEP: fixture/static reader suite (layer 0)
    mvl_e2e_oracle.rs             # NEW: path-parameterized oracle tests / helpers
    mvl_e2e_liberado.rs           # NEW: Liberado producer adapter (gated on 0.6)
fixtures/trace_contracts/         # KEEP: sample JSONL
```

Optional later: `crates/mvl-conformance` binary for foreign harnesses (`mvl-conformance --mvl …`).

CI: fixture suite always on; Liberado e2e on when emission is present; foreign adapters optional
or manual (roadmap 5 / backlog 0.7).

---

## Provider mock strategy

| Requirement | Approach |
|---|---|
| No live model | `MockProvider` (or harness-native scripted completions) only |
| Deterministic multi-turn | Preload completion queue: tool_calls → stop; optional third turn after tool withdrawal |
| Known tool outcomes | Use in-process / fake tools with fixed return strings for honesty checks |
| No network | Deny outbound provider URLs in test config; fail if a real provider is selected |
| Crash case | Separate process (`std::process::Command` of a small test binary or `cargo test -- --exact` child) so the test runner survives the kill |

Do **not** require OpenRouter, DeepSeek, or any API key for this suite. Live paid dogfood remains
outside this plan (roadmap 5 / existing live scaffolds).

---

## Staging relative to backlog 0.6 / roadmap 4b

| Work item | Blocked on emission? | Notes |
|---|---|---|
| Oracle helpers + path-based suite over fixtures | No | Can land immediately; strengthens rules 4–5–7 automation beyond today's implicit checks |
| External harness adapter docs + CLI sketch | No | Consumes files only |
| Liberado producer e2e (mock provider → real MVL path) | **Yes** (or test-only emitter at the **same** API) | Without append-flush emission, "e2e" would only re-read hand-written fixtures |
| Crash survival process kill | Yes (needs real writer) | |
| Tool honesty against real tool layer | Yes | |
| Cross-harness baseline runs | Roadmap **5** / backlog **0.7** | Out of scope for first PR; oracle must stay portable |

The plan for a later coding goal should land in this order: **oracle extensions → Liberado
adapter behind 0.6 (or co-land with 0.6) → document foreign adapter**.

---

## Non-goals (this plan and the first implementation PR)

- Changing normative text of `model-view-log.md` except a pointer to this plan from the
  Conformance fixture note.
- Expanding the execution-log event set.
- Replacing `mvl_conformance.rs` fixtures.
- Live provider dogfood or full multi-harness A/B (roadmap 5).
- Implementing 0.6 emission itself inside the test plan PR (unless intentionally co-landed).

---

## Acceptance for a later implementation PR

1. Oracle runs against on-disk MVL JSONL without Liberado session types.
2. All eight Conformance bullets have automated judges (fixture and/or e2e).
3. Liberado path (when enabled) uses mock/scripted completions only and writes real append-flushed logs.
4. Fixture suite remains green; e2e does not delete it.
5. Document how a non-Liberado harness points the same oracle at its log files.

---

## References

- Spec: [`model-view-log.md`](../spec/reference/model-view-log.md), [`execution-log.md`](../spec/reference/execution-log.md)
- Reader + fixtures: `crates/test-support` (`trace_contracts`, `tests/mvl_conformance.rs`)
- Roadmap / backlog: **4a** / contracts landed; **4b** / **0.6** emit; **5** / **0.7** cross-harness baseline
- Precedent for mock-provider e2e: [`live-conformance-suite.md`](live-conformance-suite.md) (daemon Tier 1);
  coding mocks: `crates/coder-agent/tests/mock_intake_e2e.rs`, `provider::mock::MockProvider`
