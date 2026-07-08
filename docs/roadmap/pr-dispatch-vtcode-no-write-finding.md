# PR-dispatch pipeline reliability — vtcode never writes files (in progress)

**Status**: Root cause not yet found. Narrowed conclusively to vtcode itself (not the model, not our
prompt/scaffolding) via a live A/B test against opencode. Four real bugs found and fixed in
`liberado-pr-dispatch-mcp` along the way; three plausible theories for the core "no writes" symptom
were investigated and ruled out with evidence. In progress — next step is digging into vtcode's
`exec --json` source with the search space now much narrower.

## Why this matters

`liberado-pr-dispatch-mcp` is the self-improvement engine's PR factory (task → vtcode agent → draft
PR → human approval). It had exactly one live data point before this session:
[ForrestThump/liberado#2](https://github.com/ForrestThump/liberado/pull/2), a single-line tooltip
change. That's not evidence of reliability, just evidence it can work on a trivial case. The first
real test — a 10-task batch of small, well-scoped WebUI features — produced **zero PRs**. This doc is
the record of diagnosing why, so a future session doesn't have to re-derive any of it.

## Timeline

### 1. Bad `target_branch` failed silently and confusingly — fixed, committed (`09a8861`)

All 10 tasks were submitted with `target_branch: "develop"`; `ForrestThump/liberado` only has `main`.
`clone_repo`'s git stderr was discarded, so every task died with a generic
`"git clone failed (check server logs for details)"` and the logs had nothing useful either.

Fix: `git_ops::branch_exists()` (a `git ls-remote --exit-code` pre-flight check) now runs inside
`submit_pr_factory_task` itself — a bad branch is rejected synchronously, by name, before a task is
even queued. `clone_repo` also now surfaces git's real stderr instead of the generic message.

### 2. `NoChanges` detection was fooled by vtcode's own scratch dir — fixed, committed (`09a8861`)

After fixing (1) and resubmitting, tasks cloned fine, vtcode reported `success`, but produced empty
diffs — and instead of failing cleanly, they proceeded to a doomed push+PR-creation attempt with an
equally uninformative `"create PR failed: create PR"`.

Root cause: `run_vtcode_and_verify`'s "did anything real change" check
(`git status --porcelain`) ran *before* `commit_pending` strips vtcode's own `.vtcode/` scratch
directory. `.vtcode/`'s mere presence as an untracked path was enough to defeat the check — it looked
like "there are changes" when there weren't any real ones, so the task slipped past the already-existing
`ExecutorError::NoChanges` gate into a downstream failure with no diagnostic value.

Fix: `remove_vtcode_scratch_dir` (already used inside `commit_pending`) is now called *before* the
status check too, in both `run_vtcode_and_verify` (initial tasks) and `execute_revision` (revision
tasks). Confirmed live: the same task now fails immediately and correctly with `NoChanges` instead of
limping to a confusing PR error.

### 3. Diagnostics were nearly nonexistent — fixed, committed (`09a8861`)

Before this pass, a "success but empty diff" outcome was a dead end: `vtcode_client.rs` captured
vtcode's full `--json` event stream into memory, extracted one summary string from it, then discarded
the rest. No turn count, no tool-call record, no persisted transcript.

Added:
- `ExecDiagnostics` (`num_turns`, `completion_subtype`, `outcome_code`, a tool-call tally, vtcode's own
  `FileChange` report) parsed from the event stream and logged on every attempt.
- The raw `--json` stdout is persisted to `{workspace_dir}-vtcode-logs/{exec_id}.jsonl` (previously
  discarded after summary extraction).
- On a hard failure, vtcode's full stderr and its per-task `HOME`/`VTCODE_CONFIG` dir (including
  `sessions/` state) are also preserved before cleanup, instead of being deleted unconditionally.

This is what turned every subsequent question in this investigation from "no idea" into a concrete,
inspectable answer — e.g. `tool_calls={"unified_file": 2, "unified_search": 4}` immediately showed
vtcode *was* calling `unified_file`, just never with a write action.

### 4. Bounded coder/critic retry loop — built, **not yet committed**

Discovered that `dispatch_config.rs`/`dispatch.yaml(.example)` already had a fully-designed but
never-wired-up `coder`/`planner`/`critic` prompt system and an `AgentConfig.max_coder_passes` field
(parsed, unused everywhere). Implemented the loop this was designed for, modeled directly on the
existing `validate_with_self_correction`/`attempt_self_correction` pattern:

- New `src/critic.rs` — a cheap `ChatClient` call (same shape as `refiner.rs`) that reviews the actual
  `git diff` against the task description and returns `Acceptable`/`NeedsRevision { issues }`.
- `run_vtcode_with_critic_loop` (`worker.rs`) wraps the single-attempt logic in a loop bounded by
  `max_coder_passes` (already defaults to 3): on `NoChanges` or a `NeedsRevision` verdict, retries with
  an amended prompt carrying the specific rejection reason forward; stops early after two consecutive
  no-progress passes rather than spending the full budget; on final exhaustion with a real (if
  imperfect) diff, still creates the PR with the critic's concerns appended to the body instead of
  hard-failing (the human-approval gate is the existing safety net for that case).
- `build_pr_body` gained an "Automated Review" section for the `NeedsRevision`-on-exhaustion case.

All live-tested and working as designed (confirmed the early-stop-after-two-no-op-passes behavior
fires correctly). **Left uncommitted** because the loop's actual value can't be assessed until the
underlying "vtcode never writes" issue is understood — no point locking in a retry loop around a
single-attempt failure mode that might be about to change shape entirely.

## Theories investigated and ruled out for "vtcode never writes"

Every real task submitted (across ~6 live attempts, multiple prompt/config variations) showed the same
core symptom: vtcode reports `completion_subtype: "success"`, calls `unified_file` and `unified_search`
several times, but never once calls `unified_file` with `action: "write"` or `"edit"` — confirmed by
reading the actual tool-call arguments out of the persisted raw JSONL, not inferred from outcome alone.

1. **Tool-name mismatch (`write_file`/`list_dir`/`read_file` vs. the real `unified_file`/
   `unified_search`).** The dormant `prompts.coder` text referenced tool names that don't exist in this
   vtcode version's toolset — a genuine bug, fixed (tool names corrected in `dispatch.yaml.example` and
   the live scratch config), but live-retested and **confirmed zero effect** — `unified_file` was still
   called with `action: "read"` exclusively, never `"write"`.
2. **Context size.** `turn.completed.usage` showed ~77K input tokens by the final turn, and I initially
   floated this as a plausible cause (a "flash"-tier model regurgitating under heavy context). **Wrong**
   — `deepseek/deepseek-v4-flash` has a 1M-token context window; 77K is nothing close to a real limit.
   Retracted.
3. **vtcode "echoing the input as its own result" (thought to be a harness bug).** `thread.completed.result`
   contained literally `"Task: Exec Task\nDescription: <our full prompt>"`. Traced this to
   `vtcode-core/src/core/agent/runner/summary.rs`'s `generate_task_summary` — **not a bug**: this is an
   intentional, always-used structured status report (Task/Description/Agent Type/Session/Model/Turns/
   Duration/Final Status/Outcome Code, plus conditional Modified-Files/Executed-Commands/Warnings
   sections) that unconditionally embeds `task.description` verbatim as one of its lines. Since
   `task.description` *is* our entire constructed prompt, the "echo" and the huge summary length are
   both fully and mundanely explained. Retracted as a bug claim.
4. **Planning-mode tool masking.** `harness_kernel.rs::filter_tool_definitions_for_mode` genuinely does
   mask `unified_file`'s action enum down to read-only actions when `planning_active` is true (confirmed
   via `mask_tool_actions_for_mode`, test `filter_tool_definitions_masks_unified_file_actions_in_planning`).
   Our `vtcode.toml` denies `exit_plan_mode`/`task_tracker`/`plan_task_tracker`, which looked like it
   could permanently strand the model in planning mode with no way out — a strong-looking hypothesis.
   **Disproven**: `PlanningWorkflowState::new()` initializes `is_active: AtomicBool::new(false)`, and
   grepping the whole `vtcode-core` source for real (non-test) callers of `.enable_planning()`/
   `enable_planning()` turns up **none** — nothing in a production code path ever activates planning
   mode. This mechanism is inert for our invocation shape. Retracted.

## Decisive test: opencode A/B (2026-07-08)

To separate "vtcode-specific bug" from "this model/task-shape is just unreliable at this" or "our
scaffolding is somehow the problem," ran the *identical* task against
[anomalyco/opencode](https://github.com/anomalyco/opencode) instead of vtcode — same model
(`openrouter/deepseek/deepseek-v4-flash`), same fresh clone of `ForrestThump/liberado`, `--auto` for
unattended write approval, `--format json`.

Installed via `npm i -g opencode-ai@latest` (published package, not a source build) — diagnostic-only,
explicitly not intended to become a project dependency (TypeScript; not this project's stack).
Scratch clone + run lives at `<session scratchpad>/opencode-test/` (session-temp, not in either repo).

**Result: opencode wrote 101 lines across 3 files in one attempt** — a correct, idiomatic Dioxus RSX
implementation (matching this exact codebase's framework), including a `#[cfg(target_arch = "wasm32")]`-
gated clipboard call via `wasm_bindgen`, a "Copied" indicator wired to a signal, correctly scoped to
user/assistant message roles only, exactly as specified. Not boilerplate — genuine codebase
understanding. vtcode, same model, same task, same repo: zero writes across every attempt.

This rules out both remaining broad explanations: the model is clearly capable of this task (opencode
proves it), and it's not something generically wrong with how the task is scoped/described (opencode
got an equivalent prompt and had no trouble). **The problem is specific to vtcode** — its `exec` mode,
its tool-calling pipeline, or something about non-interactive headless invocation — not the model, not
our surrounding prompt construction or dispatch-server scaffolding.

## Current state

- Committed to `liberado-pr-dispatch-mcp`, local `master` only (3 ahead of `origin/master`, not
  pushed): `5157b21` (Windows portability), `3d0f43f` (vtcode-fork cleanup), `09a8861` (branch
  validation + NoChanges ordering fix + diagnostics).
- **Uncommitted**: the coder/critic retry loop (`src/critic.rs`, `config.rs`, `constants.rs`,
  `git_ops.rs`, `lib.rs`, `vtcode_client.rs`, `worker.rs`, `dispatch.yaml.example`, `.gitignore`).
- `vtcode-src/` — a fresh clone of `vinhnx/VTCode` (includes the earlier-merged `#697` fix), built
  locally at the `liberado-pr-dispatch-mcp` repo root, gitignored. Used for source investigation and as
  the scratch dispatch server's `VTCODE_BIN`. Not a dependency of the actual project.
- No vtcode-side fix yet. Next session should resume source investigation inside `vtcode-src/`, now
  scoped specifically to headless `exec --json` invocation — something between "the model decides what
  to call" and "the write actually lands" is different between vtcode and opencode for this exact
  scenario, and that gap hasn't been located yet.

## Related docs

- [`docs/ideas/handoff.md`](../ideas/handoff.md) — current-state summary, updated after each session arc.
- [`docs/roadmap/human-todo.md`](human-todo.md) — action items blocked on the user (uncommitted work,
  PR review, etc.).
