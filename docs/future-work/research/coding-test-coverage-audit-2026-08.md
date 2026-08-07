# Coding Pack Test Coverage Audit (2026-08-07)

**Branch:** `coding-test-coverage` · **Tool:** `cargo llvm-cov` · **Scope:** 6 coding crates

---
## Overall coverage

| Crate | Lines | Covered | % | Key gap |
|---|---|---|---|---|
| coder-core | 1,521 | ~1,200 | ~79% | tuning validation, verifier deser |
| coder-agent | — | — | — | verify_pipeline, critic, retry loop, strategist |
| coder-tools | 8,125 | 5,202 | **64%** | 3 git tools + background jobs (0%) |
| coder-sandbox | 3,909 | 2,909 | **74%** | HostWorkspace/DockerWorkspace::run_command (0%) |
| executor | 5,887 | 4,544 | **77%** | parallel reads, converse_messages, mixed batches |
| coder-runner | — | — | — | mostly wiring (headless task path tested via harness-bench) |

---
## Critical gaps (0% coverage, high blast radius)

### 1. `HostWorkspace::run_command` — lib.rs:356–390 — **35 lines, 0%**
Every tool invocation on a real host workspace flows through this. No test spawns an actual process or checks stdout/stderr capture, timeout enforcement, or exit-code collection. Only `ensure_command_allowed` (the policy gate) is tested in isolation.

### 2. `DockerWorkspace::run_command` — lib.rs:206–236 — **31 lines, 0%**
Same situation. `docker_run_args` (argument assembly) is tested, but the actual `docker run` invocation, timeout, output capping, and `kill_on_drop` are never exercised.

### 3. `git_log` — tools/lib.rs:777–813 — **36 lines, 0%**
Accepts `limit`, `branch`, `format` params. No test at all. Agent can't use it in real workflows without risk of silent breakage.

### 4. `git_fetch` — tools/lib.rs:815–838 — **23 lines, 0%**
Like git_log, accepts `remote` and `branch` params. Also missing the `starts_with('-')` security guard that git_push and git_branch have. Agent can inject arbitrary git flags.

### 5. `git_merge` — tools/lib.rs:840–866 — **27 lines, 0%**
Supports `branch` + `fast_forward_only` flag. Zero coverage. Also missing the `starts_with('-')` guard.

### 6. `run_command_background` + `check_background` — tools/lib.rs:868–961 — **93 lines, 0%**
The entire async background job subsystem — spawning, job-ID tracking, polling for completion, lock-poisoning error paths, unknown-job error. Completely untested.

## High-impact medium gaps

### 7. Parallel read-only tool execution — executor/lib.rs:936–978 — **~43 lines, 0%**
Every mock runtime inherits `fn is_read_only() -> false`. The `futures::join_all` path for batching read-only tools is never exercised. No test verifies concurrent execution.

### 8. `converse_messages` — executor/lib.rs:603–639 — **37 lines, 0%**
Public API for multi-turn chat over pre-existing message history. Differs from `converse` (no scratchpad injection, caller-owned messages). Zero direct tests.

### 9. Verifier pipeline — verify_pipeline.rs:142–311 — **~170 lines, ~25% covered**
5 verifier spec types exist. Only `PathsExist` + `ContentContains` happy path is tested. Missing:
- `paths_absent` pass/fail (0 tests)
- `command` check (pass/fail/timeout/spawn-fail → 0 tests)
- `git_nonempty_diff` (uncommitted/last-commit/empty → 0 tests)
- `fail_fast` policy early-break (0 tests)
- `errors_are_failures: false` not cascading (0 tests)

### 10. Multi-attempt retry loop — agent/lib.rs:103–213 — **0 tests for retry**
All tests succeed on attempt 0 or use `max_attempts = 1`. The retry-with-prior-feedback path (NoChanges, Validation error) is never exercised in integration tests.

---
## What IS well-tested (celebrate these)

| Area | Coverage | Notes |
|---|---|---|
| hashline.rs | 88.9% | Comprehensive parse/apply/commit/rem tests |
| risk_gated.rs | 91.6% | Escalation, doom-loop detection, recovery bonus |
| preflight.rs | 97.9% | PR-create guard, branch validation |
| PathPolicy/PathDenyList | ~95% | All deny-patterns tested |
| executor::budget | 92.2% | Turn/wall-clock/token limits all tested |
| coder-core::verify (structs) | 88.0% | PipelineResult, Verdict, Finding types well-covered |
| repo_map.rs | 58.5% | Graph building, tag extraction, language detection all tested; rendering functions too; gaps are in `walk_source_files` depth + personalization edge cases |

---
## Top 10 actions by effort ÷ impact

| # | What | Lines | Effort | Blast Radius |
|---|---|---|---|---|
| 1 | Test git_log, git_fetch, git_merge with `--` guard | ~90 | Low | Blocks all git-heavy agent workflows |
| 2 | Test run_command_background + check_background roundtrip | ~90 | Medium | Async job system — used by long-running tasks |
| 3 | Test HostWorkspace::run_command (spawn, timeout, capture) | ~40 | Medium | **Every** tool call on host sandbox |
| 4 | Test verify_pipeline paths_absent, command, git_nonempty_diff | ~80 | Medium | All 4 missing verifier types |
| 5 | Test parallel read-only execution (is_read_only → join_all) | ~50 | Medium | Core engine feature — speed win |
| 6 | Test converse_messages with pre-existing history | ~40 | Low | Public API — regression risk |
| 7 | Test multi-attempt retry loop (NoChanges → success) | ~60 | High | Core reliability mechanism |
| 8 | Test critic parsing (parse_critic_verdict, extract_json_object) | ~30 | Low | Hot path, zero tests |
| 9 | Test tuning::validate for planner/critic/repair roles | ~30 | Low | Config load-time safety |
| 10 | Test VerifierSpec deser for all 5 variants | ~40 | Low | Only 1/5 covered |

---
## Notes

- The `liberado-coder-runner` (headless CLI) is tested implicitly via harness-bench live runs, not via Rust unit tests.
- `repo_map.rs` at 58.5% is mostly rendering + personalization edge cases — the core algorithm (extract, graph, PageRank) is well-covered.
- The ACP bridge (`acp-bridge/src/main.rs`) is a skeleton and not included in coverage targets.
- Coverage was measured with `cargo llvm-cov --lib` across all 6 crates. Binaries (main.rs files) are excluded from lib coverage.
