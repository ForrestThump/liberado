# Model View Log (MVL) — v1

**Status**: Spec, 2026-08-11. Normative for Liberado; proposed as the common target for the other
harnesses we benchmark against (Kilo Code, pi, Hermes, Deep Agents).

## What this is for

Comparing harnesses is only meaningful if every harness answers the same question the same way:
**what did the model actually see, and what did it do about it?**

That question has cost us real time. A trace that recorded what the model *returned* but never what
it was *sent* left the system prompt unrecoverable from any run. A trace written only on anticipated
exit paths lost 46 of 122 tool calls — precisely the attempt that failed in an unexpected way. Both
were fixed by finding the gap the hard way. This spec exists so the next harness we instrument does
not rediscover them.

It is **not** an internal event log. Harness-specific machinery — guard internals, scheduler state,
retry bookkeeping — stays out. The test for inclusion is: *did this change what the model saw, or
what it produced?*

---

## Format

**JSONL**, one event per line, UTF-8, LF.

**Append and flush as you go.** Do not buffer the run and write at the end. The most valuable log is
the one from the run that crashed, and a log assembled at exit is exactly the log you lose.

Every line carries the envelope:

| Field | Type | Notes |
|---|---|---|
| `v` | int | Spec version. `1` for this document. |
| `type` | string | Event type, below. |
| `ts` | string | RFC3339 UTC, millisecond precision. |
| `run` | string | Run id, stable for the whole run. |
| `seq` | int | Monotonic from 0. Ordering authority — two events can share a `ts`. |

Unknown fields must be preserved by readers and ignored. Unknown `type` values must be skipped, not
treated as errors: a v1 reader has to survive a v1.1 producer.

---

## Events

### `run_started`

```json
{"v":1,"type":"run_started","ts":"…","run":"…","seq":0,
 "harness":{"name":"liberado","version":"0.1.0"},
 "model":{"id":"deepseek/deepseek-v4-pro","provider":"openrouter"},
 "task":{"id":"P3.2","text":"…"},
 "repo":{"commit":"9302cd0","dirty":false},
 "config":{"loop.doom_threshold":3,"edit.fuzzy":true}}
```

`config` is the **resolved** knob values in force, not a profile name. A profile edited later must
not silently rewrite what this run ran with.

### `tool_catalog` — the definitions offered to the model

```json
{"v":1,"type":"tool_catalog","sha256":"…",
 "tools":[{"name":"grep","description":"Search files", "input_schema":{"type":"object","properties":{"pattern":{"type":"string"}},"required":["pattern"]}}]}
```

`tools` is the ordered list of complete definitions sent to the provider: name, description and
input schema. The digest is over canonical JSON for that list. Emit the body once per distinct
digest and refer to the digest from every `prompt`. A list of names is not enough: description and
schema changes can change model behaviour even when the names stay fixed.

### `prompt` — what the model sees

```json
{"v":1,"type":"prompt","turn":7,
 "messages":{"mode":"delta","items":[{"role":"tool","content":"…"}]},
 "system":{"sha256":"…","text":null},
 "tool_catalog_sha256":"…",
 "tools_offered":["read_file","grep","edit_file"],
 "params":{"temperature":0.0,"max_tokens":8192}}
```

- `messages.mode` is `full` or `delta`. **A `full` is required at the first prompt and immediately
  after any `context_changed`;** every other prompt may be a `delta` of messages appended since the
  previous one. This is not an optimisation detail — logging the entire conversation every turn is
  O(n²) and reproduces in the log the exact cost problem the log exists to study.
- `system.text` appears **once per distinct `sha256`**, `null` thereafter. The hash appears every
  time, so "did the prompt change mid-run" is always answerable.
- `tool_catalog_sha256` identifies the complete ordered definitions on this request.
- `tools_offered` is what the model could reach **on this request**. Recording it after the response
  is a different fact: guards withdraw tools mid-run, and only the request-time list explains the
  choice the model then made.

### `completion` — what the model produced

```json
{"v":1,"type":"completion","turn":7,
 "text":"Let me check the call sites.",
 "tool_calls":[{"id":"c1","name":"grep","arguments":{"pattern":"handle_request"}}],
 "finish_reason":"tool_calls",
 "usage":{"input":18422,"cached_input":17800,"output":96,"reasoning":0}}
```

`usage.cached_input` is required when the provider reports it. Cost claims are unfalsifiable without
it, and a cache that silently stopped working looks identical to one that never worked.

### `tool_result` — what the model was shown back

```json
{"v":1,"type":"tool_result","turn":7,"call_id":"c1","name":"grep","ok":true,
 "content_shown":"crates/acp-bridge/src/main.rs:412: …",
 "full_content":{"ref":"sha256:…","bytes":48210,"path":".liberado/offload/…"},
 "truncated":false,"offloaded":true,"duration_ms":312}
```

`content_shown` is **exactly the string handed to the model** — after truncation, after offload,
after any rewriting. If the harness replaced a 48 KB result with a path and a preview, that
replacement is what goes here, and `full_content` says where the rest went.

`ok` is the harness's own success flag. Where a tool can report success while achieving nothing —
a write that touched no bytes, a patch that matched nothing — `ok` must reflect the *verified*
outcome, not the absence of an exception.

### `context_changed` — what the model stopped seeing

```json
{"v":1,"type":"context_changed","turn":22,"kind":"compaction",
 "removed_messages":31,"summary":{"ref":"sha256:…"},
 "details":{"read_files":["a.rs"],"modified_files":["b.rs"]}}
```

`kind` is one of `compaction`, `eviction`, `offload`, `reset`. The next `prompt` must be `full`.

### `tools_changed` — a guard altered the catalogue

```json
{"v":1,"type":"tools_changed","turn":20,"removed":["edit_file"],"added":[],
 "reason":"doom_loop:remove"}
```

Separate from `prompt.tools_offered` on purpose: the prompt says *what was available*, this says
*that something took it away and why*. A run where the model ends up unable to finish because its
tools were withdrawn is otherwise very hard to read, and we have had exactly that run.

### `run_ended`

```json
{"v":1,"type":"run_ended","outcome":"failed","reason":"critic returned empty content",
 "gates":[{"name":"cargo test","passed":false}]}
```

`outcome` is one of `succeeded`, `failed`, `cancelled`, `aborted`. **`aborted` means an unhandled
error, distinct from `failed`, which is a decision.** They read alike in a summary and could not be
less alike to debug.

A run that crashes must still emit this. If the process cannot, the reader treats a log with no
`run_ended` as `aborted` with an unknown reason — but a producer that relies on that is
non-conforming.

---

## Boundary with execution telemetry

The MVL stays limited to the request/response view. A companion execution log uses the same
`run`, `turn` and `call_id` values and records harness state that did not enter the model request:
attempt boundaries, tool start/finish, retries, context-transform work, gates, resource use and
worker-graph edges. This second stream is where concurrency and scheduler policy are measured.

The streams must join without time-order guesses. Do not add scheduler fields to the MVL to avoid
writing the execution log, and do not reconstruct the model view from execution events. This split
keeps the MVL portable while preserving enough detail to compare graph and loop implementations.

---

## Large payloads and secrets

**Content references** replace any payload over a producer-chosen threshold:
`{"ref":"sha256:<hex>","bytes":<n>,"path":"<optional>"}`. The digest is over the raw bytes. Two runs
that read the same file produce the same ref, which makes cross-run comparison cheap.

**Never log:** API keys, `Authorization` headers, or the process environment. A producer that strips
anything must record `"redacted":["env"]` on the event, so a reader can tell "absent" from "removed".
A sanitized export that removes message or tool content does not satisfy exact reconstruction. Keep
the raw log access-controlled and retention-bounded; mark derived exports as sanitized.

---

## Conformance

An adapter conforms when it satisfies all of:

1. **Reconstruction.** From the log alone, a reader can rebuild the exact message list, system text,
   ordered tool definitions and sampling parameters sent at any turn N. This is the test that
   matters — if it fails, the log is not "what the model sees", it is a summary of it.
   `full`/`delta` correctness falls out of this.
2. **Crash survival.** Kill the process mid-run; the log up to that point is valid JSONL and every
   line parses.
3. **Ordering.** `seq` is gap-free and monotonic.
4. **System prompt recoverable.** Every distinct system prompt appears in full exactly once, and
   every `prompt` carries its hash.
5. **Tool catalogue recoverable.** Every distinct ordered tool catalogue appears in full exactly
   once, and every `prompt` carries its hash.
6. **Tool honesty.** `content_shown` byte-equals what the tool layer handed the model.
7. **Withdrawal visible.** Any change to the offered tool set appears as `tools_changed`.
8. **Join integrity.** Every execution event that refers to a turn or call joins to one MVL event by
   id; no sequence or timestamp inference is needed.

A shared conformance suite should own these, so an adapter is verified rather than asserted — pi's
telemetry package does exactly this and it is the right pattern.

**Fixture location (Liberado):** sample JSONL and reconstruction tests live under
`crates/test-support` (`trace_contracts` module + `tests/mvl_conformance.rs`). They prove a reader
can rebuild messages, system text, ordered tool definitions and sampling params for any turn from
the log alone. They do **not** emit production logs — emission is a separate backlog item.

**End-to-end conformance plan:** producer-driven integration tests (mock provider, multi-harness
oracle over on-disk JSONL, all eight Conformance rules) are specified in
[`docs/future-work/mvl-e2e-integration-test-plan.md`](../../future-work/mvl-e2e-integration-test-plan.md).
That plan is implementation guidance, not part of this normative contract.

### Reconstruction checklist (normative for fixtures)

For any turn `N` present in the log, a conforming reader must recover:

| Artifact | How |
|---|---|
| System text | Latest non-null `prompt.system.text` whose `prompt.system.sha256` matches the hash on turn `N`'s `prompt` (or the text embedded when first seen). |
| Ordered tool definitions | Full `tool_catalog.tools` for the digest in `prompt.tool_catalog_sha256`. |
| Message list | Apply every `prompt` with `messages.mode=full` as a reset, then append each subsequent `delta` until turn `N` inclusive. |
| Sampling parameters | `prompt.params` on turn `N` (temperature, max_tokens, and any other keys the producer set). |
| Tools offered on the request | `prompt.tools_offered` on turn `N`. |

If any of these are missing or contradictory, the log fails reconstruction.

---

## Mapping from what we have

Liberado's `CoderEvent` stream is a useful source, but the common emitter belongs at the executor /
provider boundary so every agent pack gets the same request record:

| MVL | Liberado today | Gap |
|---|---|---|
| `tool_catalog` | tool names inside `ModelRequestSent` | add complete ordered definitions and hash |
| `prompt` | `ModelRequestSent` (#117) | add the message delta, catalogue hash and sampling params |
| `completion` | `ModelTurnFinished` | add `usage.cached_input` |
| `tool_result` | `ToolFinished` | add `offloaded` / `full_content` |
| `tools_changed` | *(inferred by diffing `tools_offered`)* | make it explicit |
| `run_ended` | `SessionFinished` / `SessionAborted` (#124) | already distinguishes decision from crash |
| `context_changed` | — | not emitted |

The format also changes: our trace is a single JSON document written at the end of an attempt.
That is why PR #124 was needed at all — and under this spec that class of bug is structurally
impossible, because the log is flushed per event.

**Kilo Code** already emits JSONL with `tool_use` carrying `state.status`, `state.input` and
`state.output`, so a converter is straightforward; what it lacks is the system prompt, which is the
single most valuable field for comparing harnesses and the one we had to add on our own side.
