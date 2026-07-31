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

## Round 2: direct source investigation inside vtcode itself (2026-07-08, same day)

Re-cloned `vtcode-src/` from **`ForrestThump/VTCode`** (the user's fork, byte-identical to upstream
`vinhnx/VTCode main` at the time — confirmed via `gh api .../compare`, `ahead_by`/`behind_by` both 0)
instead of upstream directly, so any fix found here can be pushed straight to a branch for a PR, matching
the pattern from the earlier "inherit" bug. `origin` on the existing built clone was just repointed
(`git remote set-url`) rather than re-cloned, since the content was identical — saved a ~15 minute
rebuild.

Built a small, permanent diagnostic aid before going further: `VTCODE_DUMP_SYSTEM_PROMPT=<path>` (added
to `runner/execute.rs`, one `if let Ok(path) = std::env::var(...)` before returning the assembled
`RuntimePromptBundle`) — writes the fully-composed system prompt to a file on every run. This is what
made the rest of this round possible; without it, everything below would have stayed speculation about
code paths instead of read directly off what the model actually receives.

Also switched to driving `vtcode.exe` **directly** (bypassing the dispatch server entirely, using a
hand-built per-task `HOME`/`VTCODE_CONFIG` dir matching `setup_task_config`'s template) for this round —
much faster iteration than restarting the whole dispatch server between each single-variable test.

### A second, distinct bug found and fixed: `orchestration_mode` default is crash-prone

The dispatch server's config never sets `agent.harness.orchestration_mode`, so it uses the `#[default]`
variant, `HarnessOrchestrationMode::PlanBuildEvaluate` (`vtcode-config/src/core/agent.rs`) — a three-phase
pipeline: a tool-less **planner** call that must return strict JSON (`spec_markdown`, `contract_markdown`,
`task_title`, `items`), the actual tool-calling **build** loop, then a skeptical **evaluator** call
("prefer failing borderline cases"). The intermittent hard crash from earlier today
(`Execution failed: parse planner response`, 3-event JSONL, no `thread.completed`) is this planner
phase's JSON response occasionally failing to parse — confirmed by reproducing it again on a direct run,
this time with full untruncated stderr (the diagnostic gap from Round 1 is now closed).

The alternative, `HarnessOrchestrationMode::Single`, skips this multi-phase JSON-round-trip machinery.
Tested: **no crash across repeated runs** — a real, confirmed fix for the crash specifically. But it did
**not** change the "never writes" symptom at all (identical tool-call pattern, same outcome) — the two
symptoms are independent bugs, not one root cause. `orchestration_mode = "single"` is worth setting in
`vtcode.toml` regardless, purely for the crash fix, once the write issue is separately resolved (setting
it now would just make failures quieter without making runs succeed).

### Further theories tested for "never writes" — also ruled out or inconclusive

5. **`task_tracker`/`plan_task_tracker` denied while the system prompt repeatedly instructs their use.**
   The dumped system prompt's "Operating Profile" section says, verbatim, "Use `task_tracker` for
   non-trivial work" and "only stop when the tracker is current and verification is resolved" — but the
   prompt's own `Runtime Tool Catalog` line confirms `task_tracker` is not among the 17 available tools
   (denied in our `vtcode.toml`, inherited from copying `vtcode-explorer.toml`'s read-only denial list
   into the main coding config, where it doesn't belong). This is a real, genuine misconfiguration —
   the model is told to use its own primary progress/completion-tracking mechanism and that mechanism
   doesn't exist for it. **Fixed it (`task_tracker`/`plan_task_tracker` → `"allow"`) and retested: no
   effect on the core symptom** — still zero writes — though this run surfaced a second, previously-unseen
   anomaly (next item). Worth keeping the allow either way; it's correct regardless of whether it moves
   the needle on writes.
6. **Tool-call parsing anomaly under parallel calls.** The system prompt explicitly instructs "Run
   independent tools in parallel." In the `task_tracker`-allowed run, several real tool calls
   (`unified_file`, `unified_search`) that were immediately followed by a second tool-call item with an
   **empty tool name** (`""`) ended up marked `status: "failed"` — even though their own arguments looked
   completely valid. Traced the actual (non-test-only — the first file found,
   `vtcode-llm/.../openrouter/stream_decoder.rs`, is entirely `#[cfg(test)]` and not what runs in
   release) streaming tool-call accumulator, `providers/shared/mod.rs::update_tool_calls` — it correctly
   reads each delta's own `"index"` field rather than trusting array position, so the obvious version of
   this bug class isn't present. Found `agent.harness.max_parallel_tool_calls` (default 4) and tested
   `= 1`: the anomaly didn't appear in that run (0 failed/empty calls) — but a **later** run (the
   `system_prompt_mode = "minimal"` test, below) showed the identical empty-name/failed pattern again
   with the same setting in place. So `max_parallel_tool_calls = 1` is **not a real fix**, just
   apparently lowered the odds in one sample — the anomaly is real and reproducible but still not
   root-caused. Left at `1` since forcing sequential calls is harmless and clearly doesn't hurt.
7. **System prompt verbosity.** vtcode ships four `system_prompt_mode` levels (`minimal` ~150-250 tokens
   through `default`'s full ~6-7k-token guidance, `vtcode-config/src/types/mod.rs`). Hypothesis: a dense,
   heavily-instruction-laden default prompt could overload a "flash"-tier model's ability to prioritize
   and act, independent of raw context-window size (already ruled out in Round 1). Tested
   `system_prompt_mode = "minimal"`: **made things worse, not better** — 14 turns (vs. 4-6 typical),
   7205 events, the same file re-read 7+ times in overlapping chunks (confused, repetitive exploration),
   multiple failed/null tool calls, still zero writes, still reported `success`. Retracted as a fix
   direction — if anything this suggests the *default* prompt's structure is load-bearing for this model,
   not excessive.

### Where this leaves it

Eight hypotheses tested end to end this session (4 in Round 1, 4 more here), two real bugs found and
fixed (the `NoChanges`-gate ordering bug from Round 1; the `orchestration_mode` crash here), one
genuine misconfiguration fixed on principle (`task_tracker` denial) even though it didn't move the core
symptom, one reproducible-but-not-root-caused parsing anomaly (parallel tool calls sometimes corrupt an
adjacent call) — and the central "vtcode never writes for this task, opencode does" symptom is still
unexplained. Every individual lever tested changes *something* (crash frequency, turn count, event
volume, which calls fail) without changing the one outcome that matters. That pattern — many real,
verifiable side-effects, no effect on the core behavior — is itself informative: this doesn't look like
a single misconfigured flag waiting to be found. It looks either like a deeper interaction (multiple
factors compounding) or a genuine model-response-handling defect in a code path none of these seven
config knobs touch.

## Round 3: diffing the actual outgoing HTTP request against opencode's (2026-07-08, same day)

Per the user's explicit direction — keep `deepseek/deepseek-v4-flash` fixed on both sides (it's
independently known-good at tool use, proven with opencode/kilocode/MCP elsewhere), and treat this as
a vtcode defect to fix, not a reason to switch models or tools long-term.

Added a second permanent diagnostic hook, `VTCODE_DUMP_HTTP_REQUEST_DIR=<dir>` in
`openrouter/provider/mod.rs::dispatch_request` — writes the exact outgoing JSON payload (pretty-printed)
to `{dir}/{nanos}.json` before every send. For opencode's side, since it has no built-in raw-request
dump, stood up a minimal local logging reverse-proxy (`opencode-test/log_proxy.py` — throwaway,
diagnostic-only, not part of either project) in front of `https://openrouter.ai`, and pointed opencode
at it via a custom `openai-compatible` provider entry in `opencode.json` (`baseURL` →
`http://127.0.0.1:8899/api/v1`). Confirmed opencode still writes correctly through the proxy before
trusting any captured data from it.

Structural differences found by direct comparison of the two request sets:

- **Tool design**: opencode sends **10 discrete, single-purpose tools** (`read`, `write`, `edit`, `bash`,
  `grep`, `glob`, `webfetch`, `todowrite`, `task`, `skill`), each with its own schema and a `required`
  array (`write`: `["content", "filePath"]`; `edit`: `["filePath", "oldString", "newString"]`). vtcode
  sends **17 tools**, several of them "unified" multi-action tools — `unified_file` bundles
  read/write/edit/patch/delete/move/copy behind one `action` enum parameter, across 14 total properties,
  **with no `required` array at all** — every field, including `action` itself, is optional, and
  `action`'s own description says "Optional; inferred from old_str/patch/content/destination/path."
  **Tested**: added `"required": ["action", "path"]` to `unified_file_parameters()`
  (`vtcode-utility-tool-specs/src/lib.rs`), rebuilt, retested live. **No effect** — model still only ever
  sent `action: "read"`, same as before. Ruled out as the (sole) cause, though arguably still worth
  keeping upstream as better API design regardless.
- **`tool_choice`**: opencode's requests explicitly include `"tool_choice": "auto"`. vtcode's requests
  **never include `tool_choice` at all** — traced to `runner/execute.rs`: it's only constructed when
  `provider_name.eq_ignore_ascii_case("openai")`, so for our `provider = "openrouter"` config the
  condition is always false and the field is always omitted. Confirmed via `to_provider_format` /
  `to_openai_format` (`vtcode-llm/src/provider/request.rs`) that the value vtcode would send if enabled
  (`ToolChoice::allowed_tools_auto(...)`) actually serializes to the exact same plain `json!("auto")`
  string opencode sends — the `AllowedTools` variant's own `tools` list isn't even used in that
  serialization path, so there was no compatibility reason for the `"openai"`-only gate.
  **Tested**: widened the gate to also match `"openrouter"`, rebuilt, retested live, **confirmed via the
  HTTP dump that `tool_choice: "auto"` now actually appears in the request**. **Still no effect on the
  core symptom** — same read-only exploration pattern, same `success` outcome, zero writes.
- Everything else structurally matched or was already covered by Round 1/2: same model string, `stream:
  true` both sides, `max_tokens` present both sides (vtcode: 2000/turn — confirmed not zero, not
  obviously starving a request), message roles/shape consistent with normal chat-completions format on
  both sides.

Total now: **10 distinct interventions tested** (8 config-level in Round 2, 2 source-level fixes in
Round 3 — the schema `required` fix and the `tool_choice` provider-gate fix), each independently
confirmed to actually take effect (verified in the live HTTP request or event log, not just assumed),
none changing the core outcome. The `tool_choice` fix in particular was the strongest structural lead
found — matches the request shape exactly, no plausible reason left to expect it *wouldn't* help — and
it didn't move the needle at all. That's a meaningfully stronger negative result than the earlier config
tests: it says the remaining gap is very unlikely to be in request-level parameters at all.

## Current state

- Committed to `liberado-pr-dispatch-mcp`, local `master` only (3 ahead of `origin/master`, not
  pushed): `5157b21` (Windows portability), `3d0f43f` (vtcode-fork cleanup), `09a8861` (branch
  validation + NoChanges ordering fix + diagnostics).
- **Uncommitted**: the coder/critic retry loop (`src/critic.rs`, `config.rs`, `constants.rs`,
  `git_ops.rs`, `lib.rs`, `vtcode_client.rs`, `worker.rs`, `dispatch.yaml.example`, `.gitignore`).
- `vtcode-src/` — cloned from **`ForrestThump/VTCode`** (`origin` repointed there from upstream
  mid-session; content was identical at clone time), built locally at the `liberado-pr-dispatch-mcp`
  repo root, gitignored. Three uncommitted local patches, all worth keeping regardless of the core
  symptom's fate:
  1. `VTCODE_DUMP_SYSTEM_PROMPT` env-var hook (`runner/execute.rs`) — dumps the composed system prompt.
  2. `VTCODE_DUMP_HTTP_REQUEST_DIR` env-var hook (`openrouter/provider/mod.rs::dispatch_request`) —
     dumps every outgoing request JSON.
  3. `unified_file_parameters()` `required: ["action", "path"]` (`vtcode-utility-tool-specs/src/lib.rs`)
     — better API design even though it didn't fix the symptom.
  4. `tool_choice` provider-gate widened to include `"openrouter"` (`runner/execute.rs`) — structurally
     correct (confirmed-safe serialization, matches opencode's shape), even though it didn't fix the
     symptom either.
- Scratch diagnostic harness at `<session scratchpad>/opencode-test/` — `vtcode-home/` (hand-built
  per-task `HOME`/`VTCODE_CONFIG` dir — **reset to the plain baseline before any new test**, no test-only
  overrides should be left in it), `vtcode-prompt.txt` (exact prompt text for reproducibility),
  `log_proxy.py` (throwaway logging reverse-proxy for capturing opencode's real requests, not part of
  either project), `opencode.json` (opencode's custom-provider config pointing at the proxy), and
  `http-dumps-*`/`vtcode-*-output.jsonl` pairs from every numbered test across all three rounds.
- **No fix yet for the core symptom**, despite 10 independently-confirmed interventions. The evidence
  base is now large enough that further blind config/request-parameter guessing has a low expected
  payoff — three real options going forward, in rough order of promise: (a) full message-history content
  diff between a vtcode turn and an equivalent opencode turn (not just top-level request keys, which
  Round 3 already covered) — specifically how tool results get echoed back into conversation history,
  since that's the one major structural area not yet compared; (b) try a different model through vtcode
  to see if the symptom is deepseek-v4-flash-×-vtcode-specific or affects every model (would need to
  violate the "keep the model fixed" constraint deliberately, as a diagnostic-only, one-off check, not a
  workaround); (c) file this as a vtcode issue/discussion with the full reproduction case and the list of
  ruled-out causes — genuinely useful to their maintainers even without a confirmed root cause, and they
  may recognize the pattern immediately from experience with other providers.

## Related docs

- [`docs/handoff.md`](../project/handoff.md) — live ops handoff (not the archived ideas/handoff snapshot).
- [`docs/roadmap/archive/human-todo.md`](archive/human-todo.md) — archived operator checklist (uncommitted work,
  PR review, etc.).
