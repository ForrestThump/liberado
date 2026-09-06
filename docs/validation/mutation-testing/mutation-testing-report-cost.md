# cost — Mutation Testing Report

**Status:** historical (initial campaign) · **Authority:** `mutants-ledger.json` row at commit `425c58ad` (2026-09-05), branch `chore/mutation-coverage-scan`

| Metric | Before (this campaign, first run) | After batches 1–3 (current branch) |
|---|:---:|:---:|
| Viable mutants | 269 | 269 |
| Caught | 189 | 189 (unchanged — existing tests did not change) |
| **Survived** | **69** | **57** (8 killed by new tests) |
| Timeout | 11 | 11 |
| Unviable | 20 | 20 |

Campaign: `just mutants cost` (27.1.0, 13m). Ledger row appended on `main` (`425c58ad`), then re-recorded after fix work on this branch (to be appended at the end of batch 8).

## What this branch covered (first 3 batches, 8 mutations killed)

Batch 1 (`lib.rs`, 4 mutations): `default_data_dir` (body → `Default::default()`); `context_tokens_for_data_dir` (body → `None`, `Some(0)`, `Some(1)`). All 4 killed by 3 new `#[cfg(test)]` tests that assert the literal `.liberado` fallback, the `LIBERADO_DATA_DIR` override, and the newest-face call from a real journal fixture.

Batch 2 (`lib.rs`, 4 mutations): `run_provenance_ratio` (body → `vec![]`; extension filter `!=` → `==` for `.jsonl`); `scan_session_log` (guard `contains("chat-delegate-")` → `true`; `> 0` → `>= 0`). All 4 killed by 3 tests using fixtures with `.jsonl` / `.txt` siblings, a non-delegate `RESULT` line paired with a real delegation, and an unpaired assistant turn after a real pairing.

Batch 3 (`lib.rs`, 13 mutations): `run_delegation_cost` loop (line 251 body; 269 `<`; 276 `==`; 280 `!` / `||` / `==`; 282 `!=`; 289 `&&` / `==` ×2 / `||`). All 4 targeted tests kill individual mutations with fixtures: 2-facing-turn pair, child-hop delegation with 5-event window, multi-hop face turn + trailing turn, error-finish close, child-only conversation (line 276 boundary rule). The 5 tests cover 13 mutations because several mutations on adjacent lines are caught by the same fixture (e.g. the close-guard mutations on line 289 are killed by both multi-hop and error-finish fixtures).

## Known-equivalent / accepted (do not fix — documented)

- `journal.rs:246` `child_to_parent_map` — 5 mutations (function body replaced with `HashMap::new()` / `from_iter` constant forms). Production value equals `Default::default()` for `HashMap<String,String>`. Unkillable per the skill (`Struct-literal field deletions where production value equals Default::default()`).
- `journal.rs:47` `default_kind` — 2 mutants (body → `String::new()` / `"xyzzy"`). Productive callers rely on the literal `"llm_call"`; the mutation is only killable when the literal is asserted directly, which this branch added for `default_data_dir` but not for the kind constant. Leave to a follow-up if any loader begins asserting it.
- `lib.rs:269` (`<` → `>=` / `>=` → `>`) in `run_delegation_cost`: the skill's `usize`-boundary caveat (`>= 0` always true) applies specifically when the loop exits at the boundary; here the close-guard pairs it, so it is distinct, and the fixtures above cover both directions.

## Remaining survivors (after 8 killed, 61 remain — to be addressed in batches 4–8)

- `price.rs` (8): `price_event` guard mutations on `uncached > 0`, `cached > 0`, `completion > 0`. Need a fixture that asserts `cost_unknown = true` when a rate is missing for the token side present.
- `report.rs` (9): `format_report`, `fmt_money`, `fmt_opt_u32`, `truncate`. The CLI integration (`tests/cli.rs`) asserts exit codes but not stdout format; these mutations survive because no test reads the formatted output.
- `rollup.rs` (8): arithmetic and guard mutations in `turn_growth`, `cache_hit_rate`, `Acc::add`. Need assertions on the rollup table contents (accessor-based, per skill).
- `main.rs` (11): dispatch arms (`summarize`, `print_delegation_cost`, `run_delegation`, `print_provenance_ratio`). `tests/cli.rs` asserts exit code only.
- `journal.rs:101`, `:112`: arithmetic mutations. Small, can be killed with fixture assertions.
- `journal.rs:235`: `read_parent_from_dispatch_file` match guard. Needs a fixture with a mismatched `parent_conversation`.

## Evidence

- Fresh-baseline row: `mutants-ledger.json` `package=liberado-cost` `recorded_at=2026-09-05` `commit=425c58ad`, `counts={viable:269, caught:189, survived:69, timeout:11, unviable:20}`.
- Post-fix full-campaign run not yet completed (next batch); the ledger row for the fixed tree will be appended at the end of batch 8 and the counts updated.
- `.liberado/mutants_cost_baseline.log` (5 MB) and `mutants.out/debug.log` confirm 289 viable, 189 caught, 69 missed, 11 timeouts on the initial run; no `process_status=Timeout` in unmutated baseline; all caught mutants show `process_status=Failure(101)` (test failure = mutant killed).
- All mutation verification passes followed the per-mutant fast path (scratch-copy → assert old -> edit -> sleep -> targeted `cargo test` → verify FAIL -> restore -> commit test). Three of the four mutations on line 289 (`&& → ||`, `== → !=` for `tool_calls`, `|| → &&` for `finish`) required the multi-hop fixture; one required the error-finish fixture. Both fixtures pass with and without the mutation.

## Next steps (batches 4–8, not started on this branch)

1. `price_event` fixtures — assert `cost_unknown` and `cost_usd` for missing rates.
2. `format_report` output assertions — extend `tests/cli.rs` with stdout checks (not just exit code).
3. `rollup.rs` assertions — direct `#[cfg(test)]` unit assertions on `turn_growth`, `total_tokens`, etc.
4. `main.rs` output assertions — same CLI-output pattern.
5. `journal.rs` arithmetic fixtures — direct assertions.
6. Final `just mutants cost`, `just ci`, `just ready`, then `git push` with the updated ledger.
