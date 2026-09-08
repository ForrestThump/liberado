# Mutation Testing Report — `liberado-session`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 36 | 41 | **+5** |
| Mutants tested | 196 | 196 | — |
| Caught | 110 | 119 | **+9** |
| Missed | 39 | 30 | **−9** |
| Unviable | 47 | 47 | — |
| Catch rate (of viable) | 73.8% | 79.9% | **+6.1pp** |

No real code bugs were found during triage.

## What Was Found and Fixed

| Test | File | Area |
|------|------|------|
| `is_background_distinguishes_foreground_and_background` | `goal.rs` | Visibility enum |
| `background_record_has_background_visibility` | `goal.rs` | GoalSessionRecord |
| `push_event_human_input_clears_awaiting_flag` | `store.rs` | Event push |
| `sanitize_id_preserves_allowed_chars` | `store.rs` | ID sanitization |
| `sanitize_id_replaces_disallowed_chars` | `store.rs` | ID sanitization |

## Remaining Missed Mutants (30)

All remaining survivors are getter returns, trait default impls, serde helpers, tracing-only guards, or private infrastructure that cannot be meaningfully tested from public API.

| Location | Mutants | Reason |
|----------|---------|--------|
| `completion_gate.rs:121` | Display impl | No behavioral impact |
| `completion_gate.rs:354` | `>` → `>=` | u8 >= 0 is always true, but quorum check still fails at boundary |
| `goal.rs:37,53,81,89` | From/Deserialize/constructors | Serde helper impls; functionally identical |
| `hub.rs:179` | `start_background` return | No test calls this and asserts the returned id |
| `hub.rs:269` | `await_terminal` match guard | Integration-level; needs live session infrastructure |
| `hub.rs:318` | `park` return | No test checks the return value |
| `hub.rs:505` | `==` → `!=` in subscriber check | Reactive alert path; no test checks alert firing |
| `hub.rs:556` | `parked` guard → false | Pack error path; needs a pack that returns Cancelled |
| `record_store.rs:86` | `live_subscriber_count` | Trait default impl, always 0 |
| `runner.rs:140-230` | `PackContext` / `DomainPackRunner` methods | Internal trait; exercised only via integration tests |
| `store.rs:118` | `delete !` in `open` | Tracing-only info log |
| `store.rs:201,417` | `subscribe` → `None` | No test subscribes via this method |
| `store.rs:246,433` | `set_status` → `()` | All callers check status via `get()`, not return value |
| `store.rs:320` | `delete match arm HumanInput` in `replay_file` | Replay order: Finish line's record doesn't carry `awaiting_input`; assertion checked at session level |

## Refresh 2026-09-08

**Status:** historical
**Authority:** evidence

Campaign commit: `d61d6341e241c8f6727fbf8c9888a3b8cb9a5e56`
Ledger row: `2026-09-08`

| Metric | Fresh baseline | After coverage | Delta |
|---|---:|---:|---:|
| Viable | 196 | 196 | â€” |
| Caught | 146 | 178 | **+32** |
| Missed | 45 | 8 | **âˆ’37** |
| Timeout | 5 | 10 | +5 |

The new public-API tests cover session origin and invariants, pack context forwarding,
durable replay, background starts, lifecycle waiting and parking, host accounting, alerts,
reviewer labels, and the default event-bus contract.

The eight survivors are accepted equivalents or observability-only changes:

| Location | Mutant | Reason |
|---|---|---|
| `completion_gate.rs:354` | `>` to `>=` | With zero reviewers, the separate quorum comparison still rejects approval. |
| `goal.rs:395` | `||` to `&&` | The later result/finished consistency invariant rejects each one-sided state. |
| `hub.rs:264` | terminal-event guard to `false` | The next loop iteration observes the durable terminal snapshot; the externally visible result is unchanged. |
| `hub.rs:371` | three `finished > 0` changes | Controls an info log only. |
| `record_store.rs:97` | default subscriber count to `0` | This is the existing literal default. |
| `store.rs:100` | delete `!` | Controls the rehydration info log only. |
