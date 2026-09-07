# cost — Mutation Testing Report

**Status:** historical, post-fix campaign recorded · **Authority:** `mutants-ledger.json` rows at commits `425c58ad` (2026-09-05) and `b59f208d` (2026-09-06)

| Metric | Before (`425c58ad`) | After (`b59f208d`) | Change |
|---|:---:|:---:|:---:|
| Viable mutants | 269 | 269 | 0 |
| Caught | 189 | 215 | +26 |
| **Survived** | **69** | **42** | **−27** |
| Timeout | 11 | 12 | +1 |
| Unviable | 20 | 20 | 0 |

The post-fix campaign reduced survivors by 27. Of those 27 mutants, 26 moved to caught and 1 moved to timeout. The after-campaign catch rate is 215 / 269, or about 79.9% of viable mutants.

## What the survivor-test work covered

- Batch 1 (`lib.rs`): `default_data_dir` and `context_tokens_for_data_dir`, including the literal `.liberado` fallback, the `LIBERADO_DATA_DIR` override, and a real journal fixture.
- Batch 2 (`lib.rs`): `run_provenance_ratio` and `scan_session_log` guards, with `.jsonl` and `.txt` siblings, delegation pairing, and an unpaired trailing assistant turn.
- Batch 3 (`lib.rs`): the `run_delegation_cost` loop and close guards, with two-turn, child-hop, multi-hop, trailing-turn, error-finish, and child-only fixtures.
- `price.rs`: `price_event` rate guards, including missing input or output rates and cached-input fallback behavior.
- `report.rs`: output assertions for `format_report`, `fmt_money`, `fmt_opt_u32`, and `truncate`.
- `rollup.rs`: arithmetic and guard coverage through multi-hop `report_from_parts` assertions.
- Batch 7 (`journal.rs`): a `load_latency_events` fixture that checks parsed event data, plus a tip-added assertion that `child_to_parent_map` collects each supplied pair.

Batch 6 is not complete. The `main.rs` dispatch arms still need CLI stdout assertions; `tests/cli.rs` currently checks exit codes only.

## Known-equivalent / accepted (do not fix — documented)

- `journal.rs:47` `default_kind` — about 2 mutants (body → `String::new()` / `"xyzzy"`). Productive callers rely on the literal `"llm_call"`; the mutation is only killable when the literal is asserted directly, which this branch added for `default_data_dir` but not for the kind constant. Leave to a follow-up if any loader begins asserting it.
- `lib.rs:269` (`<` → `>=` / `>=` → `>`) in `run_delegation_cost`: the skill's `usize`-boundary caveat (`>= 0` always true) applies specifically when the loop exits at the boundary; here the close guard pairs with it, so it is distinct, and the fixtures above cover both directions.

## Remaining survivors

The recorded post-fix campaign has 42 survivors. The tip adds `child_to_parent_map` coverage after that campaign, so the ledger count remains historical until the next complete run. The recorded survivors are grouped here because the current evidence does not support a precise per-file census:

- `main.rs` dispatch arms, mainly CLI behavior that needs stdout assertions rather than exit-code-only checks.
- The accepted `default_kind` mutation in `journal.rs`, as documented above.
- Other residual survivors that need a follow-up campaign and triage before they can be classified precisely.

## Evidence

- Before row: `mutants-ledger.json`, `package=liberado-cost`, `recorded_at=2026-09-05`, `commit=425c58ad`, `counts={viable:269, caught:189, survived:69, timeout:11, unviable:20}`.
- After row: `mutants-ledger.json`, `package=liberado-cost`, `recorded_at=2026-09-06`, `commit=b59f208d`, `counts={viable:269, caught:215, survived:42, timeout:12, unviable:20}`.
- Tip coverage is present in `crates/cost/src/survivor_tests.rs` for batches 1–3, `price_event`, report formatting helpers, rollup arithmetic through `report_from_parts`, the Batch 7 journal loader fixture, and `child_to_parent_map` constant-return replacements.

## Next steps

1. Add stdout assertions for the `main.rs` CLI dispatch arms in `tests/cli.rs`.
2. Run a follow-up `liberado-cost` mutation campaign and triage the residual survivors.
3. Keep known-equivalent mutations accepted unless production behavior makes a direct assertion useful.
