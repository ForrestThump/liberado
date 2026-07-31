# Mutation Testing Report — `liberado-executor`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 72 | 78 | **+6** |
| Mutants tested | 198 | 198 | — |
| Caught | 135 | 139 | **+4** |
| Missed | 33 | 29 | **−4** |
| Unviable | 30 | 30 | — |
| Catch rate (of viable) | 80.4% | 82.7% | **+2.3pp** |

No real code bugs were found during triage.

## Tests Added

| Test | Area |
|------|------|
| `cosine_of_identical_vectors_is_one` | Similarity — identity check |
| `cosine_of_orthogonal_vectors_is_zero` | Similarity — orthogonality |
| `cosine_of_zero_vector_is_zero` | Similarity — zero-vector guard |
| `args_similarity_default_near_duplicates` | Similarity — near-duplicate args |
| `args_similarity_empty_both_is_identical` | Similarity — both empty |
| `args_similarity_neutral_args_still_use_cosine` | Similarity — distinct non-empty |
| `tf_idf_smoke` | TF-IDF vector computation |

## Remaining Missed Mutants (29)

| Category | Mutants | Reason |
|----------|---------|--------|
| **Constant string getters** (6) | `wrap_up_directive` (2), `tools_removed_nudge` (2), `LoopProfile::semantic` (1), `held_summary` (2) | No test asserts the exact string content of these prompt/display helpers |
| **Constructor / public wrapper** (3) | `LoopProfile::semantic → Default::default()` (1), `converse_messages` return (2) | Default impl is identical; converse_messages return depends on provider response not asserted in tests |
| **Budget arithmetic** (10) | `run_loop` line 731 (`- → +/`), 734 (`== → !=`), 791 (`>` → `==</`>=`), 799 (`delete !`), 895 (`+= → *=`), 928 (`delete !`), 930 (`+= → -=/`*=`) | Tracing-only guards (`> 0`, `delete !`) or operators whose output diverges gradually; tests pass because they assert on tool execution outcomes not exact turn budgets |
| **TF-IDF / cosine operators** (4) | `tf_idf_vectors` `+= → -=` and `*= → +=` (2), `cosine` `|| → &&` (1) — note: `cosine * → /` and `* → +` were CAUGHT by new tests | The `||` → `&&` path is indistinguishable because both produce 0.0 for orthogonal/empty vectors |
| **args_similarity `&&` → `||`** (1) | Line 1247 | Both paths produce same result when vectors are orthogonal or one is empty |
| **RiskGatedToolRuntime guards** (5) | `authority_decision → ()` (1), zone-restriction guard → true (1), `delete !` on default WriteClass (1), Write match arm deletion (1), `held_summary → constants` (2) | Integration-level; authority_decision return value is not checked by callers |
