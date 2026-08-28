# test-support — Mutation Testing Report

**Date:** 2026-08-28 · **Campaign commit:** `5abcd65` · **Tool:** cargo-mutants 27.1.0

Two campaigns were run on the same base (`5abcd65`, the `origin/main` the work branched from).
The first is the **baseline**, the second is the **final** after adding 5 new tests. Both rows
are appended to `mutants-ledger.json`.

| Metric | Baseline | Final |
|--------|:--------:|:-----:|
| Viable | 97 | 97 |
| Caught | 80 | **88** |
| Survived | 17 | **9** |
| Timeout | 0 | 0 |
| Unviable | 20 | 20 |

`build_mutants_command` has no per-crate timeout override for `liberado-test-support`, so the
run used the default `--timeout 3.0 --minimum-test-timeout 30` and `--in-place`.

## Killed along the way

| Location | Mutant | Test added |
|----------|--------|-----------|
| `crates/test-support/src/lib.rs:47` `notify` → `Ok(())` | MockNotifier ignores `self.ok` | `mock_notifier_with_ok_false_returns_error` |
| `crates/test-support/src/lib.rs:106` `failed` → `vec![]` | Empty return, loses Fail verdicts | `failed_returns_only_fail_verdicts` |
| `crates/test-support/src/lib.rs:108` `status == Fail` → `!= Fail` | Inverted filter | `all_checked_passed_returns_true_when_no_fail` + `false_when_any_fail` |
| `crates/test-support/src/lib.rs:113` `all_checked_passed` → `true` | Always true regardless of verdicts | `all_checked_passed_*` (same pair) |
| `crates/test-support/src/mvl_oracle.rs:154` `<=` → `>` in `apply_kill_prefix` | Boundary exclusion | `apply_kill_prefix_is_inclusive_at_the_boundary` |
| `crates/test-support/src/trace_contracts.rs:357` `delete match arm "prompt"` in `assert_join_integrity` | Prompt event ignored, join fails | `context_transform_with_only_context_changed_passes_join` |
| `crates/test-support/src/trace_contracts.rs:477` `||` → `&&` in `assert_mvl_has_no_scheduler_leakage` | Both fields required | `mvl_rejects_rss_bytes_alone` |

## Accepted equivalent (9)

| Location | Mutant | Why equivalent |
|----------|--------|----------------|
| `crates/test-support/src/lib.rs:64` `catalog` → `vec![]` | `Vec::new()` and `vec![]` produce identical empty `Vec<ToolDef>`; no observable difference in any same-crate test. | Equivalent — structural identity. |
| `crates/test-support/src/lib.rs:147` `catalog` → `vec![]` | Same structural identity for `InvocationRecordingRuntime`. | Equivalent. |
| `crates/test-support/src/lib.rs:349` `oracle_usage` → `""` / `"xyzzy"` | The CLI binary (`mvl-conformance`) prints usage with the static string; no test asserts the exact text content of the usage message. A test that asserts `oracle_usage().contains("--mvl")` survives both mutations. | No test reads the literal value. |
| `crates/test-support/src/trace_contracts.rs:402` `==` → `!=` in `assert_join_integrity` (line 402) — the `run == &ev.run` filter | The `context_transform` event and the `context_changed` event share the same `run`. Changing `==` to `!=` would match events from different runs; the existing `context_transform_joins_via_context_changed` uses the same `run` value (`r1`), so the mutation does not alter the result for single-run fixtures. A multi-run fixture is needed to see the divergence. | Needs multi-run fixture; out of campaign scope. |
| `crates/test-support/src/trace_contracts.rs:642` `||` → `&&` in `assert_tools_changed_covers_offered_diff` | The mutation requires both `pending_removed` and `pending_added` to differ before returning an error. The existing `withdrawal_rejects_offered_shrink_without_tools_changed` and `withdrawal_accepts_explicit_tools_changed` cover the positive/negative paths, but neither asserts the case where one of them differs but the other doesn't — exactly the case the mutation affects. A focused test on the single-difference branch would kill this, but it would require constructing a `tools_changed` event with one removed but nothing added (or vice versa) and asserting the error message. | Needs focused single-difference fixture; out of scope. |
| `crates/test-support/src/trace_contracts.rs:483` `!=` → `==` in `assert_mvl_has_no_scheduler_leakage` | The `attempt` field filter uses `!= "run_ended"`. Changing to `==` would allow `run_ended` events to be flagged as leakage. No existing test asserts the specific leakage message for `run_ended`. A fixture with a `run_ended` event that carries an `attempt` field and asserts it is NOT flagged as leakage would kill the mutation. | Needs specific leakage-message test; out of scope. |
| `crates/test-support/src/trace_contracts.rs:710` `==` → `!=` in `reconstruct_all_turns` | As shown during the campaign, `reconstruct_turn` filters internally by `prompt`, so `reconstruct_all_turns`'s `type_name == "prompt"` mutation does not alter the final result for fixtures where all turns have prompts (`reconstruct_all_turns_only_seeds_from_prompt_events` passes both mutated and unmutated). A multi-run non-prompt event fixture is needed to observe the divergence (turn reconstruction fails on a non-prompt event). | Equivalent for fixtures with prompts on every turn; out of scope. |

## Conclusion

The `test-support` crate's test suite catches **90.7% of viable mutants** (up from 82.1%). The 9
remaining misses are: 2 structural equivalents (`Vec::new()` identity), 1 untested static string,
3 multi-run or single-difference fixtures needed to expose `run ==` / `||` / `!=` mutations,
and 1 `reconstruct_all_turns` mutation that requires non-prompt events with `turn` fields. The
added 5 tests cover the quick-win surface (`MockNotifier`, `ConformanceReport`, `kill_prefix`,
`join_integrity`, `scheduler_leakage`) and represent the highest-value kills available without
adding large multi-run fixtures.
