# Coverage Gap Analysis — New Coding Features (develop vs main)

Generated 2026-08-06 from `cargo llvm-cov --workspace` on branch `feat/coverage-gap-analysis-coding-features`.

**Baseline:** `develop` at `892f173`, compared against `main`. Covers ~6,470 new lines across 55 files.

---

## Summary: Coverage by New Feature Area

| Feature | Files | Line Cov | Risk |
|---------|-------|----------|------|
| Fan-out parallel subagents | `fanout.rs` (1110L) | 81.97% | MED |
| Git gates (change detection) | `gates.rs` (132L) | 89.91% | MED |
| Policy resolution (plan/explore) | `policies.rs` (256L) | 95.64% | LOW |
| Ship preflight hook | `preflight_hook.rs` (254L) | 86.22% | MED |
| Build phase (fanout integration) | `build.rs` (+269L new) | 65.66% | **HIGH** |
| Session pack coordinator | `session_pack.rs` (+141L) | 47.01% | **HIGH** |
| Shadow git checkpoints | `checkpoint.rs` (369L) | 90.13% | MED |
| Parent-side merge | `merge.rs` (422L) | 87.45% | **HIGH** |
| Preflight runner | `preflight.rs` (432L) | 89.59% | MED |
| Intake/DTOs | `intake.rs` (+98L) | 85.34% | MED |
| Coding tools | `coder-tools/lib.rs` (+243L) | 92.57% | LOW |
| Slash commands (focus) | `liberado-commands/` (+340L) | 54-93% | MED |
| Goal API endpoints | `goals.rs` (+606L) | *(in server totals)* | **HIGH** |
| Session hub | `hub.rs` (+39L) | 91.17% | MED |
| Config (project auth, preflight) | `config.rs` (+618L) | 92.72% | MED |

---

## High-Priority Gaps (Untested Logic)

### 1. `read_conflict_sides()` — **Completely Untested**
- **File:** `crates/coder-sandbox/src/merge.rs:188`
- **Impact:** Central to LLM-assisted conflict resolution during fan-out merge-back. Returns ours/theirs file content for the LLM to reconcile. If `git show :2:` or `:3:` fails, the LLM receives empty strings silently (`unwrap_or_default()`), potentially causing data loss.
- **What to test:** Happy path (real conflict), git-show failure fallback, empty file content.

### 2. `validate_branch_name()` / `validate_safe_name()` — **Completely Untested**
- **File:** `crates/coder-sandbox/src/merge.rs:288,302`
- **Impact:** Security boundary preventing path traversal and git injection from attacker-controlled branch/worktree names. Validates: empty, `..`, `/`, `\`, starts with `-`, spaces, double-slash.
- **What to test:** Each rejection case individually. The branch name is derived from incoming subtask payload.

### 3. `GoalContract::apply_to_request()` — **Completely Untested**
- **File:** `crates/coder-core/src/intake.rs`
- **Impact:** Mutates a `CoderRunRequest` with frozen contract data. Never called in any test. This is how the frozen intake contract gets applied to the backend request.
- **What to test:** Verifies the request is properly populated with verifiers, command policy, path policy, and that non-trivial verifier lists clear `validation_command`.

### 4. Goal API Endpoints — **Most Untested**
- **File:** `crates/server/src/api/goals.rs`
- **Untested endpoints:** `goals_domains`, `goals_list`, `goals_get`, `goals_cancel`, `goals_park`, `goals_rewind`, `goals_stream`, `goals_diff`
- **Impact:** 8 of 11 goal endpoints have zero HTTP tests. Core lifecycle (cancel, park, rewind) untested.
- **What to test:** At minimum: park/cancel/rewind happy paths + error paths (404, 409, 403).

### 5. Hub Shutdown Drain — **Completely Untested**
- **File:** `crates/session/src/hub.rs`
- **Functions:** `park_all_in_flight()`, `force_park_still_hosted()`, `start_background()`, `await_terminal()`
- **Impact:** Critical shutdown path. If hub doesn't drain properly on daemon stop, sessions leak or data is lost.
- **What to test:** Start a session, call park_all, verify sessions are parked; test force_park on hosting sessions.

### 6. Fan-out Build-Phase Integration — **Untested Through Build Phase**
- **File:** `crates/coder-agent/src/session_pack/build.rs` (lines 146-295)
- **Impact:** The fan-out block in `run_build_phase` is tested at the `fanout.rs` module level but not exercised when `CodingSessionPack::run()` takes the fan-out path. The integration with preflight-in-fanout is untested.
- **What to test:** Send a payload with `subtasks`, verify the full integrated flow through `run_build_phase` — fan-out + preflight + merge-back.

### 7. Mid-Build Resume with Checkpoint Restore — **Untested**
- **File:** `crates/coder-agent/src/session_pack.rs` (lines 206-248)
- **Impact:** `can_resume` is tested but the actual `sg.restore(&id)` path that restores files into a workspace is never verified.
- **What to test:** Create a checkpoint, restart a session pack, verify files are restored.

### 8. `cap_log()` — **Completely Untested**
- **File:** `crates/coder-sandbox/src/preflight.rs:292`
- **Impact:** Truncates preflight step output to a max byte length. Has a UTF-8 char-boundary loop. A panic here crashes the preflight gate.
- **What to test:** Truncation at multi-byte UTF-8 boundary, exact-max-length string, empty string.

### 9. `/profile` Slash Command — **Zero Coverage**
- **File:** `crates/liberado-commands/src/handlers/profile.rs:11` — **0.00%**
- **Impact:** The `/profile` command variant exists in the enum, catalog, and dispatch but has zero parse or dispatch tests. Dead code or untested feature.
- **What to test:** Parsing and dispatch.

### 10. `parse_name_status_line()` — **Completely Untested**
- **File:** `crates/coder-agent/src/gates.rs:194`
- **Impact:** Parses `git diff --name-status` output for committed-file detection. Handles added/deleted/modified/renamed files. Zero test coverage.
- **What to test:** Each status code: `A`, `D`, `M`, `R100`, `C...`, empty lines, malformed lines.

---

## Medium-Priority Gaps (Partially Tested)

| # | File | Function | Issue |
|---|------|----------|-------|
| 11 | `merge.rs:115` | `merge_branch()` | Merge-fail-without-conflicts path untested |
| 12 | `merge.rs:95` | `remove_worktree()` | Fallback path (remove fails → `remove_dir_all` + prune) untested |
| 13 | `checkpoint.rs:45` | `ShadowGit::open_or_init()` | Session-id with `\`, canonicalize failure, idempotent re-open untested |
| 14 | `checkpoint.rs:166` | `ShadowGit::list()` | Limit clamping (0→1, 150→100), malformed log lines, git-failure |
| 15 | `fanout.rs:671` | `llm_resolve_file()` | Fence stripping (code fences with/without language tags), empty content error path |
| 16 | `preflight_hook.rs:102` | `steps_from_payload()` | Empty name/run, missing timeout, empty array, non-array JSON |
| 17 | `preflight.rs:240` | `run_step_shell()` | Spawn error (`PreflightError::Spawn`), timeout path never triggered |
| 18 | `lib.rs:437` | `run_attempt()` | Completion gate (`gate.enabled`) integration path untested |
| 19 | `lib.rs:169` | `run()` | Strategist path (consecutive_refutations + directive) untested |
| 20 | `build.rs:149` | `run_build_phase()` | Fan-out child nesting refusal (fanout_child + subtasks) untested |
| 21 | `intake.rs` | `profile_verifiers()` | Unknown profile fallback (`_ => Vec::new()`) untested |
| 22 | `intake.rs` | `sanitize_draft()` | `PathsAbsent` with empty paths, `ContentContains` with empty fields |
| 23 | `config.rs` | `Config::validate()` | ~80% of validation branches untested (provider names, model roles, cron, pools, hooks, session profiles) |
| 24 | `topology.rs` | `SessionProfile` methods | `empty()`, `component_key()`, `declares_authority()`, `declared_capabilities()` all untested |
| 25 | `topology.rs` | Preflight config types | `ProjectPreflightConfig`, `PreflightProfileConfig`, `PreflightStepConfig` all untested |
| 26 | `coder-tools/lib.rs` | `from_sandbox_with_session()` | Worktree variant path completely untested |
| 27 | `coder-tools/lib.rs` | `preflight_gh_pr_create()` | `--base=X` syntax, `git ls-remote` error path untested |
| 28 | `goals.rs` | `spawn_return_handoff()` | Parked session path, ULID parse failure, chat-disabled path untested |
| 29 | `hub.rs` | `resume()` | `can_resume=false` path, `NotPermitted` path untested |
| 30 | `focus.rs` | `join()` / `spawn()` | Non-empty id/domain+goal handling untested |

---

## Low-Priority Gaps (Trivial/Builder Functions)

| # | File | Function | Issue |
|---|------|----------|-------|
| 31 | `gates.rs:16` | `command_request()` | Trivial struct builder, no standalone test |
| 32 | `policies.rs:117` | `parse_path_policy()` | Non-mode real JSON path untested |
| 33 | `policies.rs:67` | `coder_prompt()` | Normal (non-plan/explore) mode path untested |
| 34 | `build.rs:712` | Human answer timeout | `None` answer → `BudgetExhausted` path untested |
| 35 | `checkpoint.rs:194` | `ShadowGit::run_git()` (sync) | Git failure path untested |
| 36 | `coder-core/lib.rs` | `CoderGateConfig::default()` | Default values never verified |
| 37 | `coder-core/lib.rs` | Mode prompt constants | `PLAN_MODE_CODER_PROMPT` / `EXPLORE_MODE_CODER_PROMPT` content never verified |

---

## Infrastructure-Bound Gaps (Hard to Unit Test)

| # | File | Function | Issue |
|---|------|----------|-------|
| I1 | `checkpoint.rs:213` | `run_git_async()` | Git process failure paths |
| I2 | `checkpoint.rs:233` | `run_git_async_stdout()` | Git process failure + stdout parsing |
| I3 | `merge.rs` | All git functions | Happy paths tested; error paths need git binary mocking |
| I4 | `gates.rs:143` | `rev_parse()` | Error path needs invalid git ref |
| I5 | `gates.rs:165` | `committed_files_since()` | git-diff failure path |
| I6 | `preflight.rs:277` | `shell_command()` | Platform-specific (cmd vs sh) |
| I7 | `lib.rs` | `take_workspace_checkpoint()` | `ShadowGit::open_or_init` + `snapshot` failure paths |
| I8 | `lib.rs:479` | `create_linked_worktree()` | git worktree add + checkout failure paths |
| I9 | `goals.rs` | `goals_stream()` | SSE streaming needs full HTTP harness |

---

## Error Swallowing (Observational)

These silently discard errors, making debugging hard if they ever fail in production:

- `fanout.rs:619`: `git merge --abort` failure silently discarded
- `fanout.rs:378`: Worktree cleanup failure silently discarded
- `build.rs:83`: Placeholder file creation silently discarded
- `build.rs:84-101`: Placeholder git add/commit silently discarded
- `merge.rs:120-123`: `git merge --abort` failure silently discarded
- `lib.rs:585-592`: Worktree cleanup + prune failures silently discarded

---

## Comparison with Previous Analysis (2026-07-29)

The prior `coverage-gaps.md` covered 8 hardened crates at ~80% overall. The new coding features on `develop` add ~6,470 lines with coverage distribution:

- **coder-agent build phase:** 65.66% — lowest of the coding crates, needs most attention
- **coder-agent session_pack:** 47.01% — coordinator module with many untested branches
- **Most new sandbox code:** 87-93% — relatively well-covered
- **Intake pipeline:** 85% — moderate, with `apply_to_request` completely untested
- **Goal API endpoints:** Most endpoints untested — needs HTTP test expansion
- **Session hub:** Core features test-covered but shutdown path completely untested

### Top 5 Recommended Actions

1. Write tests for `read_conflict_sides` + `validate_branch_name`/`validate_safe_name` (security + data integrity)
2. Write tests for fan-out integration through `run_build_phase` (highest volume untested logic)
3. Write HTTP tests for goal endpoints: park, cancel, rewind (core lifecycle)
4. Write test for `GoalContract::apply_to_request()` (frozen contract application)
5. Write tests for hub shutdown drain (`park_all_in_flight`, `force_park_still_hosted`)
