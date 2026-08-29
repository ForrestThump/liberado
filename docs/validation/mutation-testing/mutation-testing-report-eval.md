# eval — Mutation Testing Report

**Status:** historical
**Authority:** evidence
**Date:** 2026-08-28 · **Campaign commit:** `b4686b87f66d48ba4074df5567fd15f3868524f8` · **Tool:** cargo-mutants 27.1.0

The original rows recorded the baseline and the first test pass under a commit that did not
contain the dirty-tree test changes. They remain append-only history. The reviewed rerun at
`b4686b87f66d48ba4074df5567fd15f3868524f8` is the current ledger row and identifies the exact
committed source after `scenarios_contains_labeled_anchors_across_categories` was added.

| Metric | Original baseline | Reviewed rerun |
|--------|:-----------------:|:--------------:|
| Viable | 22 | 22 |
| Caught | 20 | **21** |
| Survived | 2 | **1** |
| Timeout | 0 | 0 |
| Unviable | 3 | 3 |

`build_mutants_command` has no per-crate timeout override for `liberado-eval`, so the reviewed
run used the default `--timeout 3.0 --minimum-test-timeout 30` and `--in-place`. It tested 25
mutants in 59s after a 22s baseline build.

## Killed along the way

| Location | Mutant | Test added |
|----------|--------|-----------|
| `crates/eval/src/scenarios.rs:92` `scenarios` → `vec![]` | empty list replacement | `scenarios_contains_labeled_anchors_across_categories` asserts four anchor names (one per `ExpectKind`) are present in the labeled set |

The labeled fixture is what the eval is tuned against. cargo-mutants's `replace scenarios with
vec![]` mutation used to return an empty list, which the `for s in &scenarios` loop in `main`
would then iterate zero times — observably identical to the unmutated run on the assertions
that existed. Asserting by anchor name kills the empty-replacement and any future mutation
that drops the routing categories.

## Survivor accepted out of scope (1)

| Location | Mutant | Why retained |
|----------|--------|----------------|
| `crates/eval/src/main.rs:98` `main` → `Ok(())` | entire `#[tokio::main] async fn main` body replaced | The function reads `DEEPSEEK_API_KEY`, constructs a real `Dispatcher`, and dispatches each labeled scenario against a live model. Its only outputs are stdout lines and an `ExitCode`. No test in this crate can call it (it requires the API key) and refactoring `main` to a testable inner function is out of scope for a campaign. Structurally unkillable from a same-crate unit test. |

## Conclusion

The `eval` crate's test suite catches **95.5% of viable mutants** (up from 90.9%). The single
remaining survivor is a `tokio::main` body whose side effects (HTTP calls, stdout) are not
reachable from any same-crate test without the live model.
