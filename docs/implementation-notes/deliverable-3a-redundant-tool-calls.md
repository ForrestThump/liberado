# Deliverable #3a — measure redundant tool calls

Branch: `feat/measure-redundant-tool-calls`

## What was done

**`crates/common/src/dispatch.rs`**
Added `repeat_calls: usize` to `Report` with `#[serde(default)]` and `skip_serializing_if`. New `Report::with_repeat_calls` builder. All existing `Report` constructors across the workspace updated with `repeat_calls: 0`.

**`crates/executor/src/lib.rs`**
- In `run_loop` (the shared turn loop), a local `repeat_calls` counter accumulates byte-exact duplicates: after each `call_history.push`, the new `(name, arguments)` is compared against all prior entries using `serde_json::Value::PartialEq` (structural equality — correct for model-generated arguments from the same schema).
- All five `Terminal::Filed(report)` exit paths (prose report, submit_report, doom-loop abort, cycle abort, budget exhaustion) set `report.repeat_calls` via `.with_repeat_calls()`.
- `execute()` logs `repeat_calls` at `tracing::info!` alongside the summary.
- The existing `call_tool_with` helper added for tests with non-empty args.

**Tests** (`crates/executor/src/lib.rs` tests module)
- `zero_repeats_reported_when_no_tool_call_was_repeated` — 0 when clean.
- `an_exact_repeat_increments_the_repeat_calls_counter` — two identical calls → 1.
- `nearly_equal_args_are_not_counted_as_repeats` — `{"q":"hello"}` vs `{"q":"world"}` are distinct.
- `a_repeated_call_is_still_executed_not_deduplicated` — both calls land on `ToolRuntime`; counting does not change behaviour.

## Gates

All three clean:
- `cargo fmt --all --check` — clean
- `cargo test --workspace` — all pass (91 executor tests + rest)
- `cargo clippy --workspace --all-targets` — only pre-existing `liberado-webui` warning

## Caveats

- **No real journal data.** The acceptance item "numbers in the PR, from a real journal or a real run" cannot be satisfied without a live `latency/events.jsonl`. The tests prove the counter works; the retrospective number needs production data.
- Field added to `Report` (a widely-used struct) means ~20 constructor sites across 5 crates were touched — all mechanical `repeat_calls: 0` additions.
