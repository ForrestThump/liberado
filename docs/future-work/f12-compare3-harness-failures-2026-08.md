---
kind: finding
status: active
authority: evidence
domain: coding-harness
canonical_for: f12-compare3-harness-failures
open_items: true
---

# Compare 3 (F12) — Liberado failure modes and harness fixes

**Status**: Evidence recorded 2026-08-13. F12 product code is PR
[#156](https://github.com/ForrestThump/liberado/pull/156). The harness fixes listed
below are **not** in that PR. Do them after #156 is green.

**Who this is for**: anyone changing the coding pack's repair loop, ship bar, or
turn budget. It is the trace read from the third sequential compare (Liberado vs
pi vs deepagents, DeepSeek v4 Flash, task F12).

**Traces**: `C:\Users\Shiloh\Code\life-os-harness-compare3\out\liberado\traces\`
(three attempts). Compare write-up:
`C:\Users\Shiloh\Code\life-os-harness-compare3\COMPARE.md`.

Related measurement history:
[`coder-harness-reliability-2026-08.md`](coder-harness-reliability-2026-08.md).

---

## The one-line version

Liberado compiled and thought on this run. It still lost on **finish**. The ship
bar said `cargo exited 101`. The model never saw rustc lines or failing test
names, ran the wrong test command, and filed success. pi finished the same task
in one pass.

Do not blame the model. The same Flash run, in pi, shipped the more complete F12.

---

## What happened (three attempts)

Caps: 30 turns × 3 attempts. Tools: `read_file`, `write_file`, `edit_file`,
`run_command` (+ executor `submit_report` / `scratchpad_write`).

| Attempt | What the model did | Ship bar |
|---|---|---|
| **0** | 30 turns of reads and edits. Never ran `cargo check`. Hit the turn budget (`partially_succeeded`). | `cargo-check` 101 |
| **1** | First `cargo check -p liberado-daemon` was 101 (compile errors left by attempt 0). Fixed them. `cargo test -p liberado-daemon` passed 70 tests. Submitted. | `cargo-check` 0, **`cargo-test` 101** |
| **2** | Tried to reproduce. Burned turns on bad argv. Crate tests still green. Submitted again. | `cargo-check` 0, **`cargo-test` 101** |

Two real bar failures, then a third attempt that repeated the second.

### Failure 1 — compile, attempt 0

The model ran out of turns before it compiled. The bar then ran `cargo check` and
got 101. Feedback to attempt 1 was only:

```
FAILURE_CLASS: command_failed
FINDINGS:
- [CommandFailed] cargo-check: cargo exited 101
```

No rustc lines. Attempt 1 had to rediscover the compile errors by running check
itself.

### Failure 2 — workspace tests, attempts 1 and 2

The bar runs workspace `cargo test`. The model ran `cargo test -p
liberado-daemon`. That passed, because Liberado kept empty `capture_paths` =
allow all, so existing daemon tests that write `note.md` at the vault root still
passed. pi moved those writes under `inbox/`. Liberado did not touch
`daemon/src/tests.rs`.

Crate suite green. Workspace bar red. That is the 101.

### Why attempt 2 did not fix it

Three problems stacked.

**1. Repair text hid the failure.**  
`format_pipeline_repair` (`crates/coder-agent/src/repair_feedback.rs`) only
sends `cargo exited 101`. `command_output_to_verdict`
(`crates/coder-agent/src/verify_pipeline.rs`) already stores a stdout/stderr
excerpt on the verdict (`log_excerpt`). That excerpt never reaches
`prior_feedback`.

**2. The model ran the wrong suite, then believed it.**  
It never ran a clean `cargo test --workspace`. On attempt 2 it ran
`cargo test --workspace 2>&1` with `2>&1` as a **cargo argument**. Cargo said
`unexpected argument '2>&1'` and exited 101. That looks like the bar 101. It is
not. Other wasted calls: `cargo -p` with no subcommand, `wc` (not on Windows),
`cmd` that printed a banner and did nothing.

**3. Its own design hid the break.**  
Empty list = allow all kept the old tests green. The model had no failing test
in front of it, so it filed success.

Stale feedback made it worse: attempt 2 still carried the attempt-0
`cargo-check` 101 **and** the attempt-1 `cargo-test` 101. Check was already
green. The model spent time on a compile problem that was gone.

---

## Proposed harness fixes

Ordered by leverage. Product F12 (fail-closed default, #156) already removes
item 5's production leak. The rest is pack work.

### 1. Put the cargo excerpt in `prior_feedback`

`format_pipeline_repair` must append `verdict.log_excerpt` for each failing
result. Last ~40 lines, or the `FAILED` test names parsed from cargo output.

This is the largest single gap vs pi: pi saw its own shell output; Liberado's
repair role saw only "101".

The verdict already has the excerpt. Do not re-run cargo to "discover" it.

### 2. Name the exact bar command in the repair hint

`CommandFailed` hint today: "Reproduce the failing command locally with tools…"

It should name the command the bar ran, e.g. `cargo test --workspace`. Add one
line: "Your `-p liberado-daemon` run is not that command."

### 3. Refuse shell tokens on `run_command`

`run_command` is argv, not a shell. `2>&1`, `|`, `&&`, and `>` as extra cargo
args should fail the tool with a short message: this is not cmd.exe; drop the
token. A 101 from `unexpected argument '2>&1'` must not look like a test
failure.

### 4. Drop stale findings on the next attempt

Once `cargo-check` is 0, do not keep the old check-101 signature in
`prior_feedback`. Carry only findings that still fail.

### 5. Fail closed by default

Shipped in #156. Empty extra `capture_paths` still scopes to `inbox_path`. An
empty whitelist is not "react to everything". That makes the existing daemon
tests fail **in-crate** if someone forgets to write under `inbox/`.

### 6. Raise the coder turn budget

Shipped default / compare 3 pin: `[coder.coder] max_turns = 30`.

Attempt 0 burned the whole 30 on search and never compiled. pi used 77
assistant turns on the same task and finished.

Proposed default: **50**. Enough to read, edit, and run the bar command once.
Not unbounded. Revisit after the next sequential compare.

A mid-run nudge is the complement, not a substitute: "you have not run
`cargo check` and you have 8 turns left."

### 7. Prompt line for filter changes

One line in `prompts/coder/coder.md`: if you add a filter to `process_change`,
existing tests write notes at the vault root. Move them or they will fail
under the new rule.

### 8. Do not spend the budget on search only

Attempt 0 never compiled. Either raise the budget (item 6) or add a progress
nudge when no `cargo` / `check` / `test` has been run by turn N − 8.

---

## What not to change

- Catalog size. Six tools (four + finish) was enough. The model never called
  `grep`. The #155 prompt and invoke refuse held.
- Reasoning tokens. MVL recorded 22 071 across the three attempts.
- HostLocal path-deps. `cargo-check` was 0 on attempts 1 and 2. That is new.

---

## Mutation / evidence notes for the next pack PR

When landing items 1–4, drive `format_pipeline_repair` with a real
`PipelineResult` that has `log_excerpt` containing a `FAILED` test name.
Assert that name appears in the returned string. Break the append, watch that
test fail, restore from a scratch copy — not `git checkout`.

For item 3, call the real `run_command` path (or the argv check it uses) with
`args = ["test", "--workspace", "2>&1"]` and assert a refuse, not a cargo
spawn.
