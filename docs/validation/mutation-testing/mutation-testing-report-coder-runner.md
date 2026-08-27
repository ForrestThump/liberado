# coder-runner — Mutation Testing Report

**Date:** 2026-08-27 · **Campaign commit:** `ce1db7a5` · **Tool:** cargo-mutants 27.1.0

Two campaigns were run on the same base (`ce1db7a5`, the `origin/main` the work branched
from). The first is the **baseline**, the second is the **final** after adding the two tests
in `crates/coder-runner/src/survivor_tests.rs`. Both rows are appended to `mutants-ledger.json`.

| Metric | Baseline | Final |
|--------|:--------:|:-----:|
| Viable | 78 | 78 |
| Caught | 69 | **72** |
| Survived | 9 | **6** |
| Timeout | 0 | 0 |
| Unviable | 13 | 13 |

`build_mutants_command` has no per-crate timeout override for `liberado-coder-runner`, so the
run used the default `--timeout 3.0 --minimum-test-timeout 30` and `--in-place` (the sibling
path deps break copy-mode). The crate is small, so each mutant rebuilt in ~2–3s.

## Killed along the way

| Location | Mutant | Test added |
|----------|--------|-----------|
| `main.rs:991` `now_unix_seconds` → `0` / `1` | constant instead of the epoch clock | `now_unix_seconds_is_a_recent_epoch` asserts `> 1_700_000_000` |
| `main.rs:1027` `push_work` → `()` | no-op instead of `git push` | `push_work_pushes_the_branch_to_origin` asserts the branch lands in a bare `origin` |

## Accepted survivors (triaged)

These remain MISSED after the final run. Each is either equivalent or only observable through
a side effect the test boundary cannot assert without flakiness or machine-state mutation.

| Location | Mutant | Why kept |
|----------|--------|----------|
| `main.rs:572` `Args::parse` delete `Some("--help") \| Some("-h")` arm | help falls through to `_` | **Equivalent.** The inner parse loop (`main.rs:594`) also matches `--help`/`--h` → `Err(usage())`, so deleting the outer arm changes no observable output. `help_as_first_arg_is_an_error` already pins this. |
| `main.rs:926` `wait_for_termination_signal` → `()` | returns immediately | **Untestable without signal injection.** The real body awaits `SIGTERM`/`Ctrl+C`; a test would have to deliver a signal to its own process. Behavior differs (premature termination) but is not reachable from any tested entry point. |
| `main.rs:524` `configure_git_safe_directory` delete `!` | warns on success instead of failure | **Infrastructure/log-gated.** Only observable via a `tracing` warning, and the function writes to the *global* git config (`git config --global`), a machine-state side effect no unit test should perform. |
| `main.rs:125/126/127` `build_task_context` delete `max_map_tokens` / `min_source_files` / `mentioned_terms` from `RepoMapOptions` | field uses `Default` | **Output not observable at the boundary.** These fields feed `repo_map::generate_repo_map`, whose textual output is not asserted by any caller-facing test; the difference is invisible at the `task_context` string the function returns. Killing them needs a repo-map output assertion that is too brittle to be worth it. |

## Ledger linkage

- Baseline row: `recorded_at` 2026-08-27, commit `ce1db7a5`, survived **9**.
- Final row: `recorded_at` 2026-08-27, commit `ce1db7a5`, survived **6**.
