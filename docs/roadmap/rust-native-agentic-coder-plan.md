# Rust-Native Agentic Orchestration Plan

> **Filename note:** path still says `coder` for link stability. The plan is for a **general agentic
> mesh**; coding is the first **domain pack**, not the product identity.

**Status**: implementation in progress, 2026-07-10  
**Architecture**: [`docs/architecture/agentic-loops.md`](../architecture/agentic-loops.md)  
**Hygiene audit**: [`agentic-mesh-hygiene-audit-2026-07-10.md`](archive/agentic-mesh-hygiene-audit-2026-07-10.md)  
**Modularity**: [`docs/architecture/modularity.md`](../architecture/modularity.md)  
**Design inputs**:
[`loop_architecture_reference_article.md`](../ideas/loop_architecture_reference_article.md),
[`doomloop_research.md`](../ideas/doomloop_research.md),
Claude Code / Codex / OpenCode / Grok Build / KiloCode harness patterns.

**We are not wrapping VTCode.** VTCode does not work reliably as a coding harness. The product is a
home-spun Liberado system built on our own `Provider` + `Executor` + `ToolRuntime` + goal-session
stack. Any remaining `vtcode` wiring in `liberado-pr-dispatch-mcp` is **legacy exit scaffolding**
only — a temporary switch so PR dispatch can keep running while `liberado-loop` becomes the default
and then the only path. It is not the architecture, not a long-term backend, and not something to
invest in.

What we *do* keep from the PR factory (not from VTCode): repo cloning, forge abstraction, task DB,
validation, revision loop, Telegram approval, and draft-PR-only human review.

It is **not only a coding-agent swap**. The product is an open-source **Rust agentic orchestration
framework** (kernel + domain packs + stores + surfaces) that:

1. Runs **goals** until success, blocker, or budget exhaustion — without drift. (Vocabulary fixed
   2026-07-12: *goal* = success-based run-to-terminal; *loop* = scheduled recurrence, see
   [`loops-plan.md`](loops-plan.md); the `liberado-loop` backend string predates this and is kept
   as a legacy config value.)
2. Separates **maker from checker** (deterministic verifiers + model critics).
3. Delegates **subagents** with capability narrowing and isolated state.
4. Exposes a **session/event backend** that TUI, WebUI, CLI, and headless workers share.
5. Uses **domain packs** for tools/verifiers/env — coding first, MCP life-ops and others next —
   without forking the kernel.

**Kernel vs coding pack** (non-negotiable framing):

| Kernel (general) | Coding pack (first implementation) |
|---|---|
| Goal session, budgets, terminals, attempts | `coder-agent` composition today |
| `Provider`, `Executor`, `ToolRuntime` | `coder-tools` + `coder-sandbox` |
| Session/event API for all surfaces | `CoderEvent` specialization → mesh later |
| Subagent + capability ∩ | worktree isolation when needed |
| Meta-loop (tuner / draft PRs) | coding eval scenarios |

If a design only works for git/diff/cargo, it stays in the pack. If a second domain would need it,
it is kernel (or must become kernel). See the hygiene audit for coupling and extraction triggers.

If you only read one architecture page first, read
[`agentic-loops.md`](../architecture/agentic-loops.md). This file is the implementation roadmap and
checkpoint log, **proving the mesh through the coding pack** that unblocks PR dispatch.

---

## Why this exists

### The vtcode failure mode

The PR-dispatch system proved the draft-PR gate and forge workflow, but the coding harness was the
weak link. The diagnosis in
[`pr-dispatch-vtcode-no-write-finding.md`](pr-dispatch-vtcode-no-write-finding.md) shows that
`vtcode` can report success while doing read-only exploration and producing no real diff, while an
OpenCode A/B with the same model and task writes the expected code. Ten confirmed interventions did
not move the core symptom. More patching around `vtcode` has low expected return.

VTCode is currently the main open-source **Rust** agentic coding TUI in the wild — and it is not a
reliable substrate. We studied it as a cautionary reference (unified mega-tools, false success with
no diff). We do **not** build on it, fork it as the engine, or treat a VTCode wrapper as the product.
Liberado owns the orchestration kernel.

### The real lesson

The important architectural lesson is not "use OpenCode" or "use Claude Code." It is that a reliable
agentic system needs:

| Requirement | Liberado answer |
|---|---|
| Explicit loop | Perceive → plan (optional) → act → verify → decide → checkpoint |
| Small tools | Discrete `ToolRuntime` catalogs, not multi-action mega-tools |
| Hard stops | Named terminal states + budgets + progress guards |
| Verifiers | Deterministic gates the model cannot rewrite; critic second |
| Durable state | Traces, attempt logs, prior_feedback, resume |
| Subagents | Capability-narrowed child goals + structured `Report` |
| Surfaces as clients | TUI/WebUI/PR factory consume events; never own the loop |
| Config-owned knobs | Prompts, models, budgets, policy, validation commands |
| Empiricism | Evals + `heuristics-tuner` meta-loop |

Liberado already has most of the **inner-loop** substrate in
[`liberado-executor`](../../crates/executor/ARCHITECTURE.md): report-mode termination, scratchpad,
budget limits, doom-loop/cycle detection, and `ToolRuntime`. This plan builds the **goal session**,
**domain tools** (coding first), and **surface contracts** around that core — not a new framework
import.

---

## North-star product shape

```
┌──────────────────────────────────────────────────────────────────┐
│  Surfaces (clients)                                              │
│  liberado TUI · WebUI · CLI · PR-factory MCP · cron/hooks · ACP? │
└───────────────────────────────┬──────────────────────────────────┘
                                │ session + event stream
┌───────────────────────────────▼──────────────────────────────────┐
│  Goal session / multi-role orchestration                         │
│  Goal · criteria · roles · subagents · verifiers · terminals     │
└───────────────────────────────┬──────────────────────────────────┘
                                │ N bounded Executor runs
┌───────────────────────────────▼──────────────────────────────────┐
│  liberado-executor  (inner tool loop)                            │
└───────────────┬───────────────────────┬──────────────────────────┘
                │                       │
        CodingToolRuntime        TurbomcpRuntime / other domains
        coder-tools+sandbox      life-ops, research, custom MCP
                │
                ▼
        PR factory (commit/push/draft PR) — consumer, not loop owner
```

**Coding is first** because PR-dispatch is the self-improvement engine and the reliability gap is
there. **Generality is not deferred indefinitely**: contracts, events, budgets, terminal states, and
`ToolRuntime` stay domain-agnostic so a non-coding goal with different MCPs can reuse the same
session machinery.

Comparable systems we intentionally rip off for harness patterns (not code):

- **Claude Code** — model/tool/result loop, permissions, hooks, subagents, session storage, UI client
- **OpenAI Codex** — automations / goal-until-done, harness stop conditions
- **OpenCode** — discrete tools, sessions/events, provider abstraction, safeguards
- **Grok Build / KiloCode-class TUIs** — live event render, goal mode, subagents, cancel
- **Karpathy / bilevel loops** — objective verifier; outer loop improves process from traces

---

## Current checkpoints (2026-07-09)

- `liberado-pr-dispatch-mcp` has a `CoderBackend` seam around modify-task coding, JSON correction,
  validation self-correction, revisions, greenfield initial coding, and greenfield cargo-test repair.
  **Production direction is `LiberadoLoopProcessBackend`** (`liberado-coder-run`). A legacy
  `VtcodeBackend` may still exist as a temporary config switch for comparison/rollback during cutover;
  it is not the target system and should be deleted once Liberado loop is default and proven.
- `crates/coder-core` defines provider-agnostic run/config/event/result contracts for the **coding
  domain** (specialization of the goal-session vocabulary in `agentic-loops.md`).
- `crates/coder-sandbox` defines the workspace/command boundary and a host-local implementation.
  It also has a first Docker command-runner scaffold that builds/runs `docker run --rm -i` commands
  from `DockerSandboxSpec`, with argv tests that do not require a live Docker daemon.
- `crates/coder-tools` exposes the first discrete coding `ToolRuntime` over file/search/git/command
  and validation tools, including an atomic multi-edit `apply_patch` tool that validates all edits
  before writing. Runtime construction honors `SandboxSpec` (host-local or Docker).
- `crates/coder-agent` runs a coding **goal session**: worker role (`coder`, or `repair` on retries)
  through `CodingToolRuntime`, config prompts, post-loop `git status` + validation gates, optional
  **critic** on real `git diff` when critic prompt is set, **in-loop progress guards** (read-only
  stall, same-tool churn, validation churn), outer **max_attempts** retry with `prior_feedback`, and
  `CoderTrace` artifacts. Planner role still not executed.
- `crates/coder-runner` exposes the same backend through `liberado-coder-run`, a JSON
  `CoderRunRequest` → `CoderRunResult` subprocess boundary — low-friction bridge for nested callers
  while in-process crates stay reusable for TUI/API.
- `liberado-config-loader` exposes `tuning.coder`/`[coder]` in `tuning.toml` as the first
  config-owned surface for backend selection, role prompts/models/budgets, sandbox/policy, validation
  command, and progress thresholds. It converts directly to `CoderRunConfig`.
- Workspace verification currently expects the nested `turbovault/` checkout to be on
  `feature/vector-db`, because root `Cargo.toml` pins `turbovault-vector` from that branch.
- Process checkpoint: after meaningful coder slices, run a debt audit for anti-patterns, coupling,
  duplication, doc drift, and missing tests. First audit fixed a status-gate coupling bug where
  backend `git status` verification could be blocked by the model-facing command policy.

**Honest MVP gap vs north star**: worker + repair retry + critic + progress guards is a real goal
session slice, still short of full multi-role planning and TUI streaming. Still missing: planner
role, model-turn events, full Docker lifecycle, streaming session API for TUI/WebUI. Missing for
generality: domain-neutral `Goal`/`Session` types (extract when a second domain needs them).

---

## Design principles

1. **General agent mesh first, coding as a domain pack.** `coder-*` crates are a pack, not Liberado's
   new center. Same provider, config, capability, proposal, and eval patterns. Kernel must stay
   usable with MCP tools alone (second-domain reusability test).
2. **Backend before frontend.** PR dispatch and tests drive headlessly first. TUI/WebUI target a
   stable session/event API later; they never own the loop.
3. **Verifier before vibes.** Automated gates (diff, validation, path policy, report consistency)
   decide success. Model critics review evidence; they do not replace gates.
4. **State compounds; stops are product behavior.** Named terminals only. Every attempt leaves resume
   state. Silent endless retry is a bug.
5. **Config owns knobs.** Prompts, models, roles, budgets, tool/sandbox policy, validation commands,
   progress thresholds — validated config, not recompiles.
6. **Small tools beat clever tools.** Prefer OpenCode-shaped discrete tools over vtcode-shaped
   unified multi-action tools.
7. **Safety is code.** Path containment, command policy, sandboxing, deletion guards, draft-PR-only
   publication, capability narrowing — not prompt requests.
8. **Context is budgeted and staged.** Catalogs and large files are on-demand. Compaction is core
   architecture, even with large context windows.
9. **Maker ≠ checker; subagents isolate.** Planner/worker/critic/repair roles and child goals get
   separate prompts/models and capability intersections.
10. **Empiricism is part of the product.** Fixture tests, live smokes, eval scenarios, and
    `heuristics-tuner` integration from the start of each reliability slice.
11. **Meta-loop proposes; humans dispose.** Outer improvement of prompts/policy is draft PR /
    proposal only. Never silently widen authority (Decision 14).

---

## Target architecture

### Layers (not just crates)

| Layer | Responsibility | Status |
|---|---|---|
| **Surfaces** | TUI / WebUI / CLI / PR MCP render events, send goals, approvals | Chat clients exist; goal-session API not yet |
| **Goal session** | Multi-role outer loop, verifiers, subagents, terminals, durable state | MVP inside `coder-agent` (single role + post gates) |
| **Inner loop** | `liberado-executor` tool turns | Production |
| **Domain tools** | Coding tools, MCP tools, future domains | Coding + MCP both real |
| **Sandbox / env** | Workspace isolation for coding; capability zones for mesh | Host + Docker scaffold |
| **Meta-loop** | Evals + heuristics-tuner + draft PRs | Tuner exists; coder evals pending |

### Coding crate shape (landed / in progress)

Start with four small crates; merge only if friction proves they are too fine-grained:

| Crate | Purpose |
|---|---|
| `coder-core` | Coding contracts: `CoderTask`, `CoderBackend`, `CoderEvent`, `CoderReport`/`CoderRunResult`, `WorkspaceRef`, `SandboxSpec`, role/config structs, trace schema. No filesystem mutation. |
| `coder-tools` | Coding `ToolRuntime`: file/search/git/command/validation, path containment. |
| `coder-agent` | Goal-session **for coding**: wires executor + tools + multi-role graph + gates + traces. |
| `coder-sandbox` | Host-local and Docker workspace/command backends. |
| `coder-runner` | Process boundary (`liberado-coder-run`) for nested callers. |

### Future extraction (when a second domain needs it)

Do **not** extract early for purity. When life-ops or another domain wants the same multi-role goal
session without coding types:

| Possible crate / home | Purpose |
|---|---|
| `agent-session` or types in `liberado-common` | Domain-neutral `Goal`, terminal states, session events, attempt log |
| existing `orchestrator` / `main-agent` | Human chat and dispatch stay; goal-mode attaches rather than forks |

Until then, `coder-core` events and `Report`/`Outcome` conversion are the practical boundary.

### PR factory destination

| Future surface | Purpose |
|---|---|
| `pr-factory-core` | Task DB, forge, clone/branch/commit/push/PR, revision loop, deletion guard |
| `pr-factory-mcp` / `code-dispatch-mcp` | Thin MCP/HTTP adapter |

`liberado-pr-dispatch-mcp` becomes a consumer of `CoderBackend` only; it must not re-own the coding
loop.

### Backend trait (coding domain)

```rust
#[async_trait]
pub trait CoderBackend: Send + Sync {
    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError>;
}
```

**Target:** `LiberadoLoopBackend` / process backend (`coder-agent` / `liberado-coder-run`).  
**Legacy only:** `VtcodeBackend` may remain briefly so cutover is reversible; delete it when
Liberado loop is the sole coding path. PR factory knows only the trait — never VTCode APIs.

### Coding tool surface (v1)

| Tool | Notes |
|---|---|
| `list_files` | Glob-aware, capped, respects ignores and workspace root |
| `search_text` | `rg`-style lexical search; capped structured output |
| `read_file` | Path + optional range; hard byte/line caps |
| `write_file` | Full-file write; path policy gated |
| `edit_file` | Exact old/new; fail with context if absent/ambiguous |
| `apply_patch` | Atomic multi-file exact replacements; validate before mutate |
| `git_status` | Porcelain + ignore/scratch filtering |
| `git_diff` | Name-only/stat/patch modes with caps |
| `run_command` | Policy-checked sandbox execution |
| `validate` | Configured validation command → structured pass/fail |
| `submit_report` | Executor finish tool (`Report`) |
| `scratchpad` | In-process working memory; not doom-looped as a workspace tool |

### Role model

Roles are config-defined. Defaults are suggestions only — no hard-coded model strings in logic.

| Role | Default intent |
|---|---|
| `planner` | Propose plan, likely files, success criteria; skippable for simple tasks |
| `coder` (worker) | Execute edits in the workspace loop |
| `critic` | Review **actual** `git diff` against task requirements (separate model/prompt) |
| `repair` | Cheap targeted fix for a known validation failure signature |
| `summarizer` | Condense traces / large tool output |
| `tool_advisor` | Optional: narrow tools/files (tool-helper / dispatcher direction) |

Per-role: provider/model, temperature, max tokens, prompt path or inline prompt, turn budget, tool
visibility, stop/progress thresholds.

### Sandbox model

- `WorkspaceBackend`: prepare, snapshot, diff, cleanup
- `CommandRunner`: policy-checked commands
- `SandboxBackend`: host-local (tests/dev), Docker (first production), later remote
- `PathPolicy` / `CommandPolicy`: containment, deny globs, timeouts, output caps

Docker first for real runs; same config style as
[`phase-4-docker-transport.md`](archive/phase-4-docker-transport.md) but a different isolation layer.

### Event and session API

Stable vocabulary (coding today as `CoderEvent`; general form in `agentic-loops.md`):

- `session_started` / `session_finished`
- `role_started` / `role_finished`
- `model_turn_started` / `model_turn_finished`
- `tool_started` / `tool_finished`
- `file_changed` / `progress_checkpoint`
- `validation_started` / `validation_finished`
- `loop_guard_triggered`
- `critic_verdict`
- `subagent_started` / `subagent_finished` (when multi-agent lands)
- `report_filed`

PR dispatch consumes these for logs. Future TUI/CLI/WebUI render the same stream as a live session.

---

## Loop shape (coding goal session)

Production coding loop:

1. **Perceive** — task, repo metadata, branch, existing diff, config, policy, previous attempts,
   validation history, relevant repo docs / skills.
2. **Plan** — optional planner pass → plan, likely files, commands, success criteria.
3. **Act** — coder (worker) inner loop with the coding tool catalog.
4. **Observe** — tool results, diff state, validation, progress metrics, loop guards.
5. **Verify** — deterministic gates **before** trusting `submit_report`:
   - non-empty real diff (or explicit no-op policy)
   - path policy / deletion guard
   - configured validation pass (or accepted failure state)
   - report file list ≈ `git diff --name-only`
6. **Critic** — separate role reviews actual diff vs criteria; issues attach to draft PR or force repair.
7. **Repair / replan** — failure-signature specific next instructions, not blind retries.
8. **Report** — terminal state + summary + artifacts + trace path.
9. **PR factory** — commit, push, draft PR, notify, human revise/approve (outside the coding backend).

### Progress and loop guards

Generic doom-loop detection lives in `liberado-executor`. Coding domain adds:

- **Read-only stall** — N turns / M tools with no diff → nudge → replan / `NoChanges`
- **Same-file churn** — repeated reads/edits without metric movement
- **Search churn** — equivalent searches, no new files selected
- **Validation churn** — same failure after repair attempt
- **Diff regression** — large unrelated deletion/expansion after repair
- **Report mismatch** — success claim vs git/validation contradiction

All thresholds config-owned and logged.

### Drift control (mandatory)

- Re-inject goal + criteria + last verifier failure each outer iteration
- Progress metrics owned by code (diff identity, validation result)
- Attempt log / `prior_feedback` for resume
- Budget exhaustion → partial report with named resource, never hang

See [`agentic-loops.md`](../architecture/agentic-loops.md) for the full drift-control list.

---

## Config surface

`[coder]` in `tuning.toml` via `liberado-config-loader::CoderTuning` → `CoderRunConfig`.

Illustrative (not final schema):

```toml
[coder]
backend = "liberado-loop" # target; "vtcode" is legacy-only if still present
trace_dir = "coder-traces"

[coder.planner]
model = "deepseek/deepseek-v4-pro"
prompt_path = "prompts/coder/planner.md"
temperature = 0.1
max_tokens = 8192

[coder.coder]
model = "deepseek/deepseek-v4-pro"
prompt_path = "prompts/coder/coder.md"
max_turns = 30

[coder.critic]
model = "deepseek/deepseek-v4-flash"
prompt_path = "prompts/coder/critic.md"
temperature = 0.1

[coder.progress]
read_only_turn_limit = 4
same_tool_limit = 3
validation_repeat_limit = 2
max_attempts = 3
event_preview_max_chars = 500

[coder.sandbox]
backend = "docker"
image = "liberado-coder:latest"
network = "none"
timeout_secs = 900

[coder.commands]
allow = ["cargo test", "cargo check", "cargo fmt", "cargo clippy", "npm test"]
deny = ["git push", "git commit", "rm -rf /"]
```

Validate prompt paths, models, sandbox backends, command policies, and unknown fields before serving.
Keep examples in `config.example/`, not source constants.

Later: a domain-neutral `[goal_session]` / role-graph config may appear when a second domain shares
the multi-role machinery. Until then, coding config is enough.

---

## Migration and phase plan

### Phase 0: Documentation and boundary ✅

- Plan + architecture docs; home-spun Liberado loop is the system; VTCode is legacy exit only.
- `CoderBackend` seam in PR dispatch so the factory calls a trait, not a harness brand.
- Adapter result type: summary, files changed, diagnostics, trace path, engine name.

### Phase 1: Core contracts ✅ / ongoing

- `coder-core` contracts, serialization tests, architecture docs.
- Coder settings through main config stack (`CoderTuning`).

### Phase 2: Tool runtime and host sandbox ✅ / ongoing

- Discrete tools, path containment, output caps, host + Docker selection.
- Unit tests for escapes, ambiguous edits, patch failures, command denial, caps.

### Phase 3: Liberado loop backend MVP 🔄

- `coder-agent` + executor wiring + no-diff + validation gates + traces + `liberado-coder-run`.
- Role-provider factory; **coder + optional critic + repair-on-retry** land; planner still pending.
- In-loop progress guards: read-only stall, same-tool churn, validation churn (config thresholds).
- Outer `max_attempts` with `prior_feedback` on NoChanges / validation / critic revision.
- Mocked tests cover happy path, guards, critic accept/reject, repair retry.
- Planner role (optional, config-skippable) + failure-signature repair routing landed (2026-07-10).
- **Next inside Phase 3**:
  - model-turn event emission (may need executor streaming hooks)
  - further repair routing polish from live eval traces

### Phase 4: PR factory integration 🔄

- Default and document **`liberado-loop`** as the coding backend (`CODING_BACKEND` / process backend
  started). Treat any remaining `vtcode` selection as deprecated rollback, then remove it.
- `LIBERADO_CODER_RUN_CONFIG_PATH` for full `CoderRunConfig` without knobs duplication.
- Keep draft PR only, human approval, revision loop.
- Preserve forge, DB, branch validation, GIT_ASKPASS, deletion guard, validation/self-correction,
  repo-context injection, critic loop — these are **ours**, not VTCode.
- E2E: Liberado backend (mock then live) edits files → draft PR.
- Delete `VtcodeBackend` / VTCode client dependency when Liberado loop is sole path.

### Phase 5: Docker sandbox 🔄

- `DockerWorkspace` argv + policy + mounts started; long-lived lifecycle still open.
- Live end-to-end smoke in a container.
- No host FS access outside mounts and configured caches.

### Phase 6: Multi-role goal session + subagents

- Implement the outer loop graph in `coder-agent` (planner/critic/repair).
- Subagent tool or orchestrator path: child goal, narrowed tools/paths, worktree isolation option,
  structured Report return, max depth/concurrency config.
- Failure-signature repair routing (validation message → repair role with tight context).
- Ensure terminal states cover success / blocked / budget / policy / needs-human / no-changes.

### Phase 7: Empirical evaluation

- `coder-eval` scenarios or `heuristics-tuner` `Coder` layer.
- Scenario classes: one-file edit; multi-file feature; test failure repair; ambiguous/no-op clean
  fail; deletion guard; read-only model redirected/fail; revision against existing branch.
- Metrics: success rate; non-empty diff rate; validation pass rate; unrelated-file touch rate;
  retries; token/wall-clock cost; disallowed command attempts; no-progress detections.
- Feed results into `heuristics-tuner` (evidence-backed prompt/model/policy proposals).

### Phase 8: Session API for TUI / WebUI

- Expose goal/coder sessions over daemon/server (HTTP/SSE style aligned with chat API).
- TUI and WebUI render sessions as **clients only**.
- Session replay from trace files without rerunning the model.
- Optional: chat turn can escalate into goal mode ("pursue until done") without a separate product.

### Phase 9: Domain generality (non-coding goals) — pigeonhole detector

- Prove the same **kernel** on a non-coding goal (e.g. multi-MCP vault task with automated success
  criteria and turn budget) **without** depending on `coder-tools` / git / sandbox.
- Extract domain-neutral Goal/Session/event types into `common` or a session crate when this work
  would otherwise copy `coder-core` or import coding crates for non-coding reasons (see modularity
  extraction trigger).
- Document domain packs: config + tool limb + verifiers + prompts = a pack; coding is pack #1.
- Converge chat `AgentEvent` and coding `CoderEvent` toward one surface vocabulary.

### Phase 10: Self-improvement / meta-loop

- Coder operates on Liberado itself through PR factory.
- Heuristics tuner generates candidate prompt/config changes as draft PRs.
- Promote interesting traces into eval fixtures.
- Architecture-critique mode writes idea docs, not code, unless separately PR-dispatched.
- System never silently widens authority.

---

## Reuse from PR dispatch

**Keep:**

- forge-client + clone credential hygiene
- SQLite task lifecycle and dependencies
- branch validation before queueing
- draft PR + publish/revise/reject
- Telegram/webhook notification
- repo-context injection (`AGENT_CONTEXT_PATHS`)
- validation + self-correction (generalized off `VTCodeClient`)
- critic loop on real `git diff`
- no-changes detection and vtcode diagnostic lessons

**Remove (do not wrap as product):**

- `VTCodeClient` / `VtcodeBackend` as any long-term coding engine
- vtcode-specific config/session assumptions
- vtcode event parsing as canonical trace schema
- prompts that mention vtcode/unified tools
- any design that treats "shell out to vtcode" as the Liberado coding architecture

---

## Documentation work

| Doc | Role |
|---|---|
| [`agentic-loops.md`](../architecture/agentic-loops.md) | Canonical loop architecture |
| This plan | Implementation roadmap + checkpoints |
| [`overview.md`](../architecture/overview.md) | Crate map + status pointer |
| [`current.md`](current.md) | High-level roadmap pointer |
| Per-crate `ARCHITECTURE.md` | Zoomed contracts |
| `config.example/` | Knobs and prompts, not source constants |
| Contributing docs | "How to run coder evals" once evaluator exists |

---

## Open design questions

Resolved leanings in parentheses; revisit with evidence.

- Extract domain-neutral session types now or when a second domain needs them?
  (**When second domain needs them.**)
- Planner output: typed JSON plan vs advisory prose?
  (**Typed but non-binding**: code validates shape; worker may deviate with traceable rationale.)
- Command allowlists: global vs repo-specific?
  (**Repo overlay on global defaults.**)
- Docker images: per-language vs general?
  (**General image first, repo override later.**)
- Large context windows vs dumping?
  (**Staged retrieval + summaries always.**)
- Should goal mode share `main-agent` conversation store or a parallel session store?
  (**Parallel goal/session store first; link IDs to chat session for UX.**)
- Subagent protocol: new tools vs reuse `DispatchSubagent`?
  (**Reuse Report/correlation model; add isolation knobs where coding needs worktrees.**)
- Does `coder-agent` keep using `Report` only, or richer `CoderReport`?
  (**Richer coding result that converts to `Report` at mesh boundaries — already the pattern.**)

---

## Near-term execution order

Do not expand scope mid-slice. Recommended sequence from **current** state:

1. ~~**PR-dispatch cutover to Liberado loop default**~~ — done 2026-07-10 (`CODING_BACKEND`
   defaults to `liberado-loop`, built-in prompt, binary resolve, `VALIDATE_CMD` → validation gate).
2. **Live smokes** on real models (one-file edit → repair → full draft PR). Use
   `scripts/smoke-liberado-coder.ps1` and PR-dispatch tasks.
3. ~~**Coder layer in `liberado-heuristics-tuner`**~~ — landed: `TUNER_LAYER=coder` scores real
   temp workspaces via `liberado-coder-agent`, beam-searches system prompts, writes proposal
   rubrics only. Expand scenarios and run live with `OPENROUTER_API_KEY` as the meta-loop.
4. Delete VTCode legacy path when Liberado is sole backend in all envs.
5. **Docker live smoke** for coding sandbox; update pr-dispatch image to ship `liberado-coder-run`.
6. Planner role (optional); streaming session events; non-coding domain proof (Phase 9).

Only after headless **Liberado** coding is reliable on real tasks should frontend work become the
bottleneck. Mesh generality is enforced by Phase 9, not by renaming crates early.

---

## First concrete slice (historical — largely done)

1. Add `CoderBackend` trait so PR dispatch does not hard-depend on one harness.
2. Add `coder-core` contracts and config structs.
3. Drive PR pipeline with Liberado (mock, then real loop).
4. Real file tools and model calls in Liberado crates.

PR factory cares only about "workspace diff + report + terminal state" — produced by **our** loop.
