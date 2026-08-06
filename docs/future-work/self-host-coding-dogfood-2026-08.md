> 🐛 Live dogfood finding — first self-host of the coding pack on the liberado repo itself
> (backlog **C2**). Partially fixed; remaining bugs still open. Linked from
> [`backlog.md`](backlog.md) Band C and [`roadmap.md`](../roadmap.md).

# Self-host coding dogfood — 2026-08-05 (C2)

**Status:** Run completed with a real PR. Findings **#1–#5 addressed on `fix/dogfood-findings-c2`**
(re-dogfood required to close). #6 (role-model tuning / cost attach) remains quality polish.
**Session id:** `01KZAJN9NMRR1THMWZM8ZSBV5P`
**PR produced:** [ForrestThump/liberado#69](https://github.com/ForrestThump/liberado/pull/69)
  (`dogfood/self-host-coding-pr` → `develop`, commit `ae9e163`, author `liberado <liberado@local>`)
**Branch stack:** `develop` (merges of open PRs #66–#68, excluding draft #2) + worktree path fix

## Why this matters

Backlog **C2** is explicit: *run one real PR end to end and write up where it fell over* — before
building more coding surface. This was that run: Liberado's own coding goal session editing the
liberado repo under `WorktreeWorkspace` isolation and opening a PR. Everything up to and including
"the change is committed and pushed" worked on the second attempt; the path to get there exposed six
bugs that unit tests never saw.

## Setup (reproducible)

1. Branch `develop` from `origin/main`.
2. Merge open PRs **#66** (project auth), **#67** (plan mode), **#68** (explore mode). Skip **#2**.
3. Local `config/topology.toml` (gitignored):

   ```toml
   [[projects]]
   name = "liberado"
   root = "C:/Users/Shiloh/Code/life-os"
   write_class = "agent_writable"
   ```

4. `cargo build -p liberado-cli` → `liberado serve <vault>` (provider with `DEEPSEEK_API_KEY`).
5. `POST /api/goals` (equivalent of `/goal in liberado …`):

   ```json
   {
     "description": "… branch dogfood/self-host-coding-pr, edit docs/roadmap.md S2 row, commit, push, gh pr create --base develop …",
     "domain": "coding",
     "max_turns": 30,
     "success_criteria": ["branch pushed", "roadmap notes dogfood", "PR open against develop"],
     "payload": {
       "project": "liberado",
       "interactive": false,
       "intake": { "enabled": false }
     }
   }
   ```

`intake.enabled = false` was required after finding #2 (see below). With intake on, the session never
reached the build phase.

## Timeline

| Time (local) | What happened |
|---|---|
| Attempt 1 | Intake on → DeepSeek structured-output failure → session **failed** in ~6s (finding #2). |
| Attempt 2 (pre-fix binary) | Build phase → `git worktree add` dies on `//?/C:/…` path (finding #1). Session **failed** immediately. |
| Fix | `ed8b910` on `develop`: strip Windows extended paths; use goal session id as worktree dir name. Rebuild + restart daemon. |
| Attempt 3 | Session `01KZAJN9NMRR1THMWZM8ZSBV5P`. Worktree created. Agent branched, edited roadmap, committed, pushed. Gate then said **no real workspace changes** (finding #3) and asked human retry/abort. |
| Human nudge | `POST /api/goals/{id}/message` with guidance to only `gh pr create`. |
| Retry | PR created (finding #5: first base was `main` because `develop` was not on the remote). Retargeted to `develop` after `git push -u origin develop`. |
| Cleanup | Goal cancelled after PR #69 was correct; prunable worktree pruned. |

## Findings

### 1. Windows worktree paths + non-unique worktree id — **fixed**

- Extended-path strip + unique session id: `ed8b910` on `develop`.
- Residual (worktrees under data dir + plain `workspace_root` on the wire): **`fix/dogfood-findings-c2`**.

**Symptom**

```text
git worktree add failed: Preparing worktree (new branch 'life-os')
fatal: could not create leading directories of
'//?/C:/Users/Shiloh/Code/worktrees/life-os/.git': Invalid argument
```

**Root cause**

1. `std::fs::canonicalize` on Windows returns `\\?\C:\…`. That string is passed to `git -C` /
   `git worktree add`. Git for Windows rewrites it to `//?/C:/…` and fails with "Invalid argument".
2. `CodingToolRuntime::from_sandbox` used the **project folder name** (`life-os`) as the worktree
   directory id and `parent.join("worktrees")` as the base — so every self-host session on this
   checkout tried to create the same `…/Code/worktrees/life-os` path.

**Where**

- `crates/coder-sandbox/src/lib.rs` — `HostWorkspace::new`, `WorktreeWorkspace::new`
- `crates/coder-tools/src/lib.rs` — `from_sandbox` / `from_sandbox_with_session`
- `crates/coder-agent/src/lib.rs` — passes `request.task.id` into `from_sandbox_with_session`

**Fix landed**

- `strip_extended_path_prefix` / `path_for_cli` — strip `\\?\` and `//?/` before any CLI path.
- Host workspace roots stored without the extended prefix after canonicalize.
- Worktree directory name = goal/task session id (ULID), not the project folder name.
- Unit test: `strip_extended_path_prefix_removes_verbatim_drive_and_unc`.

**Still worth doing (not blocking)**

- Prefer `<data>/worktrees/` (or `.liberado/coding-worktrees/`) over a sibling of the project root
  (`…/Code/worktrees/`), so self-host does not drop agent worktrees next to other checkouts.
- Config `authorize_coding_workspace` injects a canonicalized `workspace_root` that still carries
  `\\?\` on Windows until stripped at the sandbox boundary — fine today, but the wire value is ugly
  in session records (`"\\\\?\\C:\\Users\\…"`).

---

### 2. Intake structured output fails on DeepSeek — **fixed (decoder + context)**

**Symptom**

```text
pack failed: intake: model/provider failed: intake complete_json: failed to decode structured
output: invalid type: sequence, expected a string — finish_reason=Stop, completion_tokens=819,
reply was 3133 chars; around the failure: … "status": "needs_clarification", "questions": [
  { "id": "workspace_path", "prompt": "What is the absolute path to the liberado workspace?" …
```

Preceded by:

```text
backend rejected the json_schema response format — falling back to json_object for this call.
Structured decoding is unconstrained until this is resolved.
error=HTTP 400: {"error":{"message":"This response_format type is unavailable now",…}}
```

**Root cause (layered)**

1. DeepSeek rejects `json_schema` response format → provider falls back to unconstrained
   `json_object`.
2. Model returns a shape that does not match `IntakeOutcome` (decode: sequence where a string was
   expected — typically a `prompt` / option field).
3. Even a valid decode would have been wrong product behaviour: intake asked for `workspace_path`
   despite `payload.project = "liberado"` and server-injected `workspace_root`.

**Where**

- `crates/coder-agent/src/intake_session.rs` — `run_intake` / `complete_json` + `intake_outcome_schema`
- `crates/provider` — json_schema → json_object fallback warning
- `crates/coder-agent/src/session_pack/intake.rs` — `IntakeSettings` (`payload.intake.enabled`)
- Server injects project: `crates/server/src/api/goals.rs` (`authorize_coding_workspace`)

**Workaround used**

```json
"payload": { "intake": { "enabled": false }, "project": "liberado" }
```

Skips intake when the grant has `AskHuman` but the operator disables the phase. Unattended sessions
without `AskHuman` already skip intake (pre-S7 path).

**What good looks like**

- Prefer a model/provider that honors `json_schema` for intake, **or** a tolerant decoder that
  coerces common model mistakes (array prompts → joined string) and re-asks once.
- Intake context must include resolved `project` / `workspace_root` so the model does not re-ask
  for a path the daemon already authorized.
- Until fixed, document `intake.enabled = false` for scripted dogfood / CI self-host runs.

---

### 3. Committed work is graded as "no real workspace changes" — **fixed**

**Symptom**

After a successful edit + `git_commit` + `git_push` on the worktree branch, the session event stream
showed:

```text
role_finished (coder)
validation_finished ok=false summary="no real workspace changes were produced"
awaiting_input: "The build did not succeed: no real workspace changes were produced
  How should I proceed? Reply with guidance, or \"abort\" to stop." options=["abort","retry"]
```

Evidence that work **was** real:

- Local branch `dogfood/self-host-coding-pr` at `ae9e163`
- `origin/dogfood/self-host-coding-pr` updated
- Diff was exactly the intended one-line roadmap note
- Commit author `liberado <liberado@local>` (from `git_commit` tool env)

**Root cause**

`LiberadoLoopBackend::run_attempt` (`crates/coder-agent/src/lib.rs`) after the worker finishes:

```text
gates::changed_files_detailed(&effective_root)  →  files_changed
if files_changed.is_empty() && report.outcome != Failed  →  CoderError::NoChanges
```

`changed_files_detailed` is effectively **uncommitted** working-tree / index diff vs HEAD. Once the
agent commits, the tree is clean → empty list → `NoChanges` → repair/ask loop. The repair hint even
says "leave a git diff", which **punishes** the correct self-host sequence (commit then push).

**Where**

- `crates/coder-agent/src/lib.rs` — `run_attempt` no-changes guard (~lines 323–352)
- `crates/coder-agent/src/gates.rs` — `changed_files_detailed`
- `crates/coder-core/src/lib.rs` — `CoderError::NoChanges`
- `crates/coder-agent/src/repair_feedback.rs` — `NoChanges` repair hint

**What good looks like**

Any of:

1. Treat "commits on this branch ahead of the workspace base / parent HEAD" as real change when
   uncommitted diff is empty.
2. Track mutations during the attempt (tool-level `file_changed` / commit SHAs) and accept those even
   if the tree is clean at the end.
3. Split success criteria: "diff vs base branch" for PR-shaped goals, not "dirty working tree".

Without this, **self-hosting that ends in a commit always fails the gate** and requires a human
retry — which is exactly the opposite of C2.

---

### 4. Tool activity barely reaches the goal event stream — **fixed (live mirror)**

**Symptom**

For session `01KZAJN9NMRR1THMWZM8ZSBV5P`, `GET /api/goals/{id}` returned only:

- `session_started`
- `role_started` / `role_finished` (coder)
- `validation_finished`
- `awaiting_input` / `human_input` / `progress` (retry)

There were **no** `tool_call`, `tool_result`, or `file_changed` session events for the tools that
actually ran (`git_branch`, edit, `git_commit`, `git_push`). Watching the dogfood live was
impossible without `git worktree list` / `git log` side channels.

**Root cause (direction)**

Coding pack / `LiberadoLoopBackend` records rich `CoderEvent`s internally (including `FileChanged`
after the no-changes check — which never ran in the success path because of finding #3). Those are
not fully mirrored onto `SessionEvent` / the SSE wire the TUI and `GET /api/goals/{id}` use.

**Where to start**

- `crates/coder-agent/src/lib.rs` — trace/`CoderEvent` emission
- `crates/coder-agent/src/session_pack/build.rs` — pack ↔ hub event bridge
- `crates/session` / `crates/server/src/api/goals.rs` — session event kinds + SSE
- Roadmap already notes gate votes batched / incomplete live wiring; this is the same class of gap
  for **tools and file changes**.

**What good looks like**

Live dogfood: `GET /api/goals/{id}/stream` shows each tool name + short args, each path written, and
each git tool result, without reading server logs.

---

### 5. `gh pr create --base develop` when `develop` is local-only — **fixed (preflight + prompt)**

**Symptom**

Agent (retry) created PR #69 with body "Authored by Liberado coding goal session (dogfood C2)." but
**base `main`**, so GitHub initially showed ~2258 additions (entire #66–#68 merge stack + fix +
docs). After `git push -u origin develop` and `gh pr edit 69 --base develop`, the PR correctly
showed **+1 / −1**.

**Root cause**

`develop` existed only locally during the run. `gh pr create --base develop` cannot target a
non-remote branch usefully; GitHub defaulted (or the tool fell back) to `main`.

**What good looks like**

- Dogfood / self-host checklist: **push the integration branch before** asking the agent for a PR
  against it.
- Optional tool hardening: if `git_push` / PR helper can see that the requested base has no
  `refs/remotes/origin/<base>`, fail with an actionable error instead of opening a wrong-base PR.
- Document in coding prompts for PR-shaped goals: "base branch must exist on origin".

---

### 6. Default coding model path is fragile for intake + observability — **open (quality)**

**Observations**

- Status API: `model_name = deepseek-chat`; coding role events: `model=session-coder`.
- Intake (finding #2) is unusable on this path without schema support.
- No cost/journal summary was pulled for this run; with `liberado-cost --json` available, the next
  dogfood should attach token/turn numbers (C5 / TE track).

**What good looks like**

- Topology `[roles.*]` for coding intake vs coding worker (cheap schema-friendly model for intake,
  stronger for build), documented in `config.example/topology.toml`.
- Dogfood write-ups always include: turns, wall-clock, outcome, PR URL, and a short
  `liberado-cost --json` excerpt.

---

## What worked (so we do not re-doubt it)

| Piece | Evidence |
|---|---|
| Project allowlist (S3/G4, PR #66) | `GET /api/projects` returned `liberado`; undeclared paths would 403 |
| Coding domain registered | `GET /api/goals/domains` included `coding` |
| Worktree isolation (after #1 fix) | `git worktree list` showed `…/worktrees/01KZAJN9…` on branch `dogfood/self-host-coding-pr` |
| `git_branch` / `git_commit` / `git_push` tools | Commit `ae9e163`, remote branch, author `liberado@local` |
| Real file edit | `docs/roadmap.md` S2 row dogfood note |
| `gh pr create` via `run_command` | PR #69 (after base fix) |
| Human mid-session message | `POST /api/goals/{id}/message` resumed retry |

C1 ("coder cannot commit") is **no longer true** on `develop` — tools landed earlier (#59); this
dogfood exercised them for real.

## Follow-ups (actionable)

| Priority | Action | Closes | State |
|---|---|---|---|
| P0 | Fix no-changes gate for committed work (#3) | #3 | **done** — `resolve_attempt_changes` + baseline SHA |
| P1 | Mirror tool + file events onto session stream (#4) | #4 | **done** — `LIVE_GATE` tool start/finish |
| P1 | Intake: schema-tolerant decode + pass authorized project into intake context (#2) | #2 | **done** — flexible prompt + auth context |
| P2 | Worktree base under data dir; strip `\\?\` on injected `workspace_root` | #1 residual | **done** — `.liberado/coding-worktrees` + server strip |
| P2 | PR helper / prompt: require remote base branch (#5) | #5 | **done** — `preflight_gh_pr_create` + coder prompt |
| P3 | Role models for intake vs worker; attach cost JSON to next dogfood (#6) | #6 | open (polish) |

## Related docs

- [`backlog.md`](backlog.md) Band C — C2 (this run), C1 (commit tools — exercised), C5 (gate measurement)
- [`coding-tui-plan.md`](coding-tui-plan.md) — S2 live dogfood was the missing acceptance item
- [`pr-dispatch-vtcode-no-write-finding.md`](pr-dispatch-vtcode-no-write-finding.md) — sibling "no writes / wrong success" class of finding for the PR factory
- [`roadmap.md`](../roadmap.md) — coding pack priority / S2 row

## Artifacts (machine-local, not committed)

- Session id: `01KZAJN9NMRR1THMWZM8ZSBV5P`
- Optional dumps: `.liberado/dogfood-goal-result*.json` (if present)
- Fix commit: `ed8b910` (`fix(coder-sandbox): strip Windows extended paths for worktree isolation`)
- Dogfood PR: https://github.com/ForrestThump/liberado/pull/69
