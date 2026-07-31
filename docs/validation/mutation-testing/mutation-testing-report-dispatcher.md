# Mutation Testing Report — `liberado-dispatcher`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

**Crate chosen:** `liberado-dispatcher` (7 dependents, 2 active source files, 48 existing tests)

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 48 | 60 | **+12** |
| Mutants tested | 72 | 72 | — |
| Caught | 40 | 53 | **+13** |
| Missed | 15 | 2 | **−13** |
| Unviable | 17 | 17 | — |
| Catch rate (of viable) | 72.7% | 96.4% | **+23.7pp** |

## What Was Found and Fixed

### 1. Boundary: Confidence floor (`guards.rs:151`)

**Missed mutant:** `replace < with <=` in `decision.confidence < tuning.clarify_threshold_write`.

No test exercised `confidence == threshold` exactly (default 0.7). The mutation would block actions with confidence precisely at the threshold — a subtle off-by-one.

**Fix:** Added `confidence_at_the_write_threshold_is_not_low_confidence` — asserts that `evaluate` passes when `confidence == tuning.clarify_threshold_write`.

### 2. Boundary: Guidance score floor (`lib.rs:299`)

**Missed mutant:** `replace < with <=` in `top.score < self.tuning.guidance_match_floor`.

No test exercised `score == floor` exactly (default 0.8). The mutation would skip the guidance short-circuit when it should fire.

**Fix:** Added `score_at_the_guidance_floor_does_short_circuit` — asserts the short-circuit fires (classifier skipped, relevant_mcps from guidance preserved) when `score == 0.8`.

### 3. `ensure_correlation` not tested (`lib.rs:765`)

**Missed mutant:** `replace ensure_correlation with ()`.

All existing `DispatchSubagent` tests used a hard-coded non-empty `correlation_id` (e.g. `"c1"`), so `ensure_correlation`'s `if correlation_id.is_empty()` was never exercised.

**Fix:** Added `ensure_correlation_mints_an_id_when_empty` — sends a `DispatchSubagent` with `correlation_id: String::new()` through dispatch and asserts the outcome has a `"sub:..."` correlation id.

### 4. `record_outcome` guard gaps (6 mutants at `lib.rs:330-344`)

**Missed mutants:** Various guard mutations in `record_outcome`'s match arms:
- Guard `!relevant_mcps.is_empty() || !seed_calls.is_empty()` forced to `true` / `false`
- Guard `!allowed_mcps.is_empty()` forced to `true` / `false`
- `delete !` on each guard expression

The existing tests only covered one path (ExecuteDirect with seed_calls). Empty ExecuteDirect, DispatchSubagent, and relevant_mcps-only paths were untested.

**Fixes:**
- `record_outcome_empty_execute_direct_is_a_noop` — empty ExecuteDirect should not record
- `record_outcome_with_relevant_mcps` — ExecuteDirect with non-empty relevant_mcps records from relevant_mcps, not seed_calls
- `record_outcome_dispatch_subagent` — DispatchSubagent with allowed_mcps records correctly
- `record_outcome_subagent_without_allowed_mcps_is_a_noop` — DispatchSubagent with empty allowed_mcps does not record

### 5. Prompt wiring: vault zones and guidance hits (`lib.rs:253`, `lib.rs:259`)

**Missed mutants:** `delete !` in `!writable.is_empty()` and `!hits.is_empty()` inside `build_request`.

Prompt construction tests for these conditional sections were absent.

**Fixes:**
- `prompt_includes_vault_zones_when_writable` — asserts the prompt contains "Vault zones" when `zone_write_classes` has an `AgentWritable` entry
- `prompt_excludes_vault_zones_when_not_writable` — asserts the prompt omits the section without writable zones
- `prompt_includes_guidance_hits_when_present` — asserts the prompt contains "Relevant past guidance" when guidance source returns hits (below short-circuit threshold)
- `prompt_excludes_guidance_hits_when_absent` — asserts the guidance section is absent without a configured guidance source

### 6. `goal_hash` constant output (`lib.rs:756`)

**Missed mutants:** `goal_hash` replaced with constant `0` or `1`.

No test asserted any property of `goal_hash`'s output. The mutations would make every goal produce the same hash, breaking correlation ID uniqueness.

**Fix:** Added `goal_hash_differs_across_distinct_goals` — asserts that two different goal strings produce different hash values.

## Remaining Missed Mutants (2 — Both Tracing-Only)

| Location | Mutation | Reason |
|----------|----------|--------|
| `lib.rs:774` | `log_classified_decision` → `()` | Tracing-only `tracing::info!` call; no return value to assert |
| `lib.rs:876` | `!=` → `==` in `mcp != entry` | Tracing-only `tracing::debug!` gate — no behavioral effect |

## Tests Added (12)

| Test | File | Area |
|------|------|------|
| `confidence_at_the_write_threshold_is_not_low_confidence` | `guards.rs` | Boundary |
| `score_at_the_guidance_floor_does_short_circuit` | `lib.rs` | Boundary |
| `ensure_correlation_mints_an_id_when_empty` | `lib.rs` | Correlation |
| `record_outcome_dispatch_subagent` | `lib.rs` | Guidance recording |
| `record_outcome_subagent_without_allowed_mcps_is_a_noop` | `lib.rs` | Guidance recording |
| `record_outcome_empty_execute_direct_is_a_noop` | `lib.rs` | Guidance recording |
| `record_outcome_with_relevant_mcps` | `lib.rs` | Guidance recording |
| `goal_hash_differs_across_distinct_goals` | `lib.rs` | Hash stability |
| `prompt_includes_vault_zones_when_writable` | `lib.rs` | Prompt wiring |
| `prompt_excludes_vault_zones_when_not_writable` | `lib.rs` | Prompt wiring |
| `prompt_includes_guidance_hits_when_present` | `lib.rs` | Prompt wiring |
| `prompt_excludes_guidance_hits_when_absent` | `lib.rs` | Prompt wiring |

## Conclusion

The dispatcher crate's test suite now catches **96.4% of viable mutants** (up from 73%). The 2 remaining misses are structurally uncatchable (tracing-only calls with no return value).
