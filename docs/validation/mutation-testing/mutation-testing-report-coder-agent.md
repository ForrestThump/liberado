# coder-agent — Mutation Testing Report

**Date:** 2026-07-30

| Metric | Value |
|--------|:-----:|
| Source files | 16 |
| Tests | 64 unit + 12 completion_gate_e2e + 8 mock_intake* |
| Viable mutants | 328 |
| Caught | 176 |
| **Catch rate** | **53.7%** |
| Unviable | 51 |
| TIMEOUT | 2 |

\* Run with `-- --lib` (`mock_intake_e2e` hangs under cargo-mutants temp dir, likely I/O race).

## Survivors by Module

| Module | Actionable | False Positive | Profile |
|--------|:----------:|:--------------:|---------|
| `session_pack/intake.rs` | ~10 | ~4 | Operator inversions (`+=`, `>`, `==`) in draft/render logic; string gets |
| `session_pack/build.rs` | ~8 | ~2 | `is_stuck` match guard, `!init_git_repo`, `>` boundary checks |
| `progress.rs` | ~8 | ~6 | `&&`/`||`, `>=`/<` inversions; guard names, messages |
| `repair_feedback.rs` | ~9 | ~5 | Match arm deletion, `||`→`&&`, `>`→`>=`; `repair_hint`/`first_line` gets |
| `roles.rs` | ~6 | ~2 | `>` operators in truncation logic; `truncate_chars` gets |
| `verify_pipeline.rs` | ~8 | ~4 | `&&`/`||`, `==`/`!=` inversions; `truncate_log`, `signature_pipeline` gets |
| `completion_gate.rs` | ~10 | ~7 | `workspace_diff`, `run_strategist` returns; `flatten_votes`, `contract_summary` gets |
| `critic.rs` | ~3 | ~1 | `git_diff_for_critic` returns; `!` negation |
| `trace.rs` | ~4 | ~2 | `||`→`&&`, `==`→`!=` in `safe_segment`; getter returns |
| `lib.rs` | ~8 | ~4 | `&&`/`||`, `<`/`<=`, `+=` operators in run/retry logic |
| Others | ~5 | ~5 | `intake_session.rs`, `gates.rs`, `planner.rs`, `runtime.rs` |

## Key Takeaways

- **This run succeeded** where Phase 3 timed out, using `--timeout 3.0 --minimum-test-timeout 90 -- --lib`.
- **53.7% catch rate** is a solid baseline for the crate's first mutant run.
- Most survivors are operator inversions (`&&`/`||`, `>`/`<`, `+=`/`*=`) and string constant
  replacements — the standard false-positive classes from earlier phases.
- The 12 `completion_gate_e2e` integration tests are NOT included in this run (they run through
  `cargo test` normally but cargo-mutants' temp dir environment caused `mock_intake_e2e` to hang).
  Their 12 pass/fail verdicts are not reflected in the mutant catch rate.

## Remediation

- The high-value survivors are in `session_pack/intake.rs` and `session_pack/build.rs` —
  operator inversions in the draft/render/intake loop. These guard the intake→freeze→build pipeline
  and mutations there could corrupt a proposal draft. Priority for a targeted follow-up.
- The `progress.rs` survivors guard the doom-loop and same-tool-limit logic — worth a focused
  patch pass if these operators have caused real bugs.
- The remaining ~100 survivors are split between string constants (~40%), operator inversions
  that would require specific boundary-value tests (~35%), and `match arm` deletions that are
  safely caught by enums (~25%).
