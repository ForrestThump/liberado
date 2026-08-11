# Execution Log — v1

**Status**: Spec, 2026-08-11. Companion to the [Model View Log (MVL)](model-view-log.md).

## What this is for

The MVL answers *what the model saw and produced*. It deliberately excludes harness machinery.

This log answers *what the harness did around that*: attempt boundaries, tool start/finish timing,
retries, context transforms, gates, resource samples, and worker-graph edges. Cross-harness
comparison of schedulers and loops needs these facts without stuffing them into the MVL.

**Join rule:** every event that refers to a model turn or tool call uses the **same** `run`,
`turn`, and `call_id` values as the MVL for that run. Readers join by id. They must not guess
from timestamps or sequence numbers alone.

---

## Format

Same envelope as the MVL:

| Field | Type | Notes |
|---|---|---|
| `v` | int | Spec version. `1` for this document. |
| `type` | string | Event type, below. |
| `ts` | string | RFC3339 UTC, millisecond precision. |
| `run` | string | Same run id as the paired MVL. |
| `seq` | int | Monotonic from 0 **within this stream**. Independent of MVL `seq`. |

**JSONL**, UTF-8, LF. **Append and flush per event** — same crash-survival rule as the MVL.

Unknown fields preserved by readers; unknown `type` values skipped.

---

## Events

### `attempt_started` / `attempt_ended`

```json
{"v":1,"type":"attempt_started","ts":"…","run":"…","seq":0,
 "attempt":0,"workspace":"/path/to/worktree"}
{"v":1,"type":"attempt_ended","ts":"…","run":"…","seq":12,
 "attempt":0,"outcome":"refuted","reason":"gate: cargo test"}
```

`attempt` is 0-based. Coding repair loops and multi-attempt builds use this; single-shot agents
emit one pair.

### `tool_started` / `tool_finished`

```json
{"v":1,"type":"tool_started","ts":"…","run":"…","seq":3,
 "turn":7,"call_id":"c1","name":"grep"}
{"v":1,"type":"tool_finished","ts":"…","run":"…","seq":4,
 "turn":7,"call_id":"c1","name":"grep","ok":true,"duration_ms":312,
 "bytes_out":48210}
```

`call_id` **must** match the MVL `completion.tool_calls[].id` / `tool_result.call_id` for the
same call. Timing and byte counts live here; the string the model saw lives only in the MVL.

### `context_transform`

```json
{"v":1,"type":"context_transform","ts":"…","run":"…","seq":20,
 "turn":22,"kind":"compaction","duration_ms":840,
 "removed_messages":31,"summary_bytes":1200}
```

`kind` is one of `compaction`, `eviction`, `offload`, `reset` — the same set as MVL
`context_changed`. Emit this when the harness *does* the work; the MVL records that the model
stopped seeing the removed messages. Both fire for the same transform, joined by `run` + `turn`.

### `retry`

```json
{"v":1,"type":"retry","ts":"…","run":"…","seq":8,
 "turn":7,"call_id":"c1","name":"edit_file","attempt":2,
 "reason":"transient_provider_error"}
```

Provider or tool retries that never reached the model as a new turn. If the model is asked again,
that is a new MVL `prompt`/`completion`, not only a `retry`.

### `gate_result`

```json
{"v":1,"type":"gate_result","ts":"…","run":"…","seq":30,
 "attempt":0,"name":"cargo test","passed":false,
 "detail":"2 failed; 0 ignored"}
```

Harness acceptance gates (verifiers, ship preflight, completion gate votes). The MVL
`run_ended.gates` may summarize; this stream is the per-gate record.

### `resource_sample`

```json
{"v":1,"type":"resource_sample","ts":"…","run":"…","seq":15,
 "cpu_ms":1200,"rss_bytes":524288000,"disk_free_bytes":2147483648}
```

Optional. Useful when disk exhaustion or memory pressure is a known failure mode.

### `worker_edge`

```json
{"v":1,"type":"worker_edge","ts":"…","run":"…","seq":2,
 "from":"parent","to":"child-3","kind":"spawn",
 "child_run":"run-child-3"}
```

Fan-out / subagent graph edges. `child_run` is the MVL `run` id of the child when it has its own
log. `kind` is one of `spawn`, `join`, `cancel`, `merge`.

### `run_linked`

```json
{"v":1,"type":"run_linked","ts":"…","run":"…","seq":0,
 "mvl_run":"…"}
```

Optional explicit pairing when the two streams use different file names but the same logical run.
When omitted, `run` equality is the join key.

---

## Conformance

An adapter conforms when it satisfies all of:

1. **Join integrity.** Every `tool_started` / `tool_finished` / `retry` with a `call_id` joins to
   exactly one MVL tool call with that id in the same `run`. Every `context_transform` with a
   `turn` joins to an MVL `context_changed` (or to the following full `prompt`) for that turn.
2. **No scheduler leakage into the MVL.** The paired MVL stream does not carry attempt counters,
   wall-clock tool durations, or worker-graph edges as required fields.
3. **Crash survival.** Kill mid-run; the log up to that point is valid JSONL.
4. **Ordering.** `seq` is gap-free and monotonic within this stream.
5. **Attempt brackets.** Every `attempt_ended` has a matching earlier `attempt_started` with the
   same `attempt` index.

A shared suite should own these fixtures so an emitter is verified rather than asserted. See
`crates/test-support` conformance tests for the fixture format.

---

## What stays out

- Full tool argument dumps (MVL `completion` already has what the model produced).
- System prompt text (MVL only).
- Provider wire bodies (optional debug elsewhere; not this contract).
