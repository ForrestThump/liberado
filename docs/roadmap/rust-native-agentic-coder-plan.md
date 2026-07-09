# Rust-Native Agentic Coder Plan

**Status**: implementation in progress, 2026-07-09. This supersedes the long-term reliance on
`vtcode` inside `liberado-pr-dispatch-mcp`, while preserving the good PR-factory pieces already built
there: repo cloning, forge abstraction, task DB, validation, revision loop, Telegram approval, and
draft-PR-only human review.

Current checkpoints:

- `liberado-pr-dispatch-mcp` has a `CoderBackend` seam around modify-task coding, JSON correction,
  validation self-correction, revisions, greenfield initial coding, and greenfield cargo-test repair.
  It can now select either the migration `VtcodeBackend` or a `LiberadoLoopProcessBackend` that
  spawns `liberado-coder-run` through the same `ExecResult`-shaped worker contract.
- `crates/coder-core` defines provider-agnostic run/config/event/result contracts.
- `crates/coder-sandbox` defines the workspace/command boundary and a host-local implementation.
  It also has a first Docker command-runner scaffold that builds/runs `docker run --rm -i` commands
  from `DockerSandboxSpec`, with argv tests that do not require a live Docker daemon.
- `crates/coder-tools` exposes the first discrete coding `ToolRuntime` over file/search/git/command
  and validation tools, including an atomic multi-edit `apply_patch` tool that validates all edits
  before writing. Runtime construction now honors `SandboxSpec`, so command execution can route
  through host-local or Docker workspaces.
- `crates/coder-agent` has an Executor-backed MVP that runs the coder role through `CodingToolRuntime`,
  loads the coder prompt from inline config or `prompt_path`, verifies real workspace changes with
  `git status --porcelain`, rejects false success reports, runs a configured validation command as a
  deterministic post-loop gate, and writes coarse `CoderTrace` JSON artifacts when `trace_dir` is
  configured. It now selects model providers through a role-provider factory seam, though only the
  coder role is executed so far.
- `crates/coder-runner` exposes the same backend through `liberado-coder-run`, a JSON
  `CoderRunRequest` -> `CoderRunResult` subprocess boundary. This is the intended low-friction bridge
  for nested/process callers such as `liberado-pr-dispatch-mcp` while the in-process loop crates stay
  reusable for a future TUI/API.
- `liberado-config-loader` exposes `tuning.coder`/`[coder]` in `tuning.toml` as the first
  config-owned surface for backend selection, role prompts/models/budgets, sandbox/policy, validation
  command, and progress thresholds. It converts directly to `CoderRunConfig`.
- Workspace verification currently expects the nested `turbovault/` checkout to be on
  `feature/vector-db`, because root `Cargo.toml` pins `turbovault-vector` from that branch.
- Process checkpoint: after meaningful coder slices, run a fresh debt audit for anti-patterns,
  coupling, duplication, doc drift, and missing tests. The first audit caught and fixed a status-gate
  coupling bug where backend `git status` verification could be blocked by the model-facing command
  policy.
- Local loop-architecture source: [`loop_architecture_reference_article.md`](../ideas/loop_architecture_reference_article.md)
  reinforces the non-negotiables for this project: automated verifier gates, durable state,
  explicit stop conditions, real tools, separated maker/checker roles, connectors into the actual
  environment, and a future outer loop that improves the inner loop using traces/evals rather than
  vibes.

**Goal**: replace `vtcode` with Liberado's own loop-based coding system, written in Rust, using the
existing `Provider` + `Executor` + `ToolRuntime` architecture as the core. The result should be a
modular backend that can power autonomous PR production first, and later a Claude-Code-like TUI/CLI
coding surface without rewriting the agent engine.

## Why This Pivot

The current PR-dispatch system proved the draft-PR gate and forge workflow, but the coding harness is
the weak link. The diagnosis in
[`pr-dispatch-vtcode-no-write-finding.md`](pr-dispatch-vtcode-no-write-finding.md) shows that
`vtcode` can report success while doing read-only exploration and producing no real diff, while an
opencode A/B with the same model and task writes the expected code. Ten confirmed interventions did
not move the core symptom. More patching around `vtcode` now has low expected return.

The important architectural lesson is not "use opencode." It is that a reliable coding agent needs:

- a simple explicit loop: perceive, reason, plan when useful, act, observe, repeat;
- small, single-purpose tools instead of a large multi-action file tool;
- hard stopping conditions, progress detection, budgets, and traces;
- an eval/tuning harness that exercises the actual loop, not only a prompt;
- config-owned prompts, models, budgets, tool policy, sandbox policy, and role assignment.

Liberado already has most of the generic loop substrate in
[`liberado-executor`](../../crates/executor/ARCHITECTURE.md), including report-mode termination,
scratchpad, budget limits, doom-loop/cycle detection, and a `ToolRuntime` abstraction. This plan
builds the coding-specific runtime around that existing core instead of importing another full agent
framework.

## Design Principles

1. **General agent mesh first, coding second.** The coder is one service in the mesh, not the new
   center of Liberado. It should use the same provider, config, trace, capability, proposal, and
   eval patterns as the rest of the system.
2. **Backend before frontend.** The TUI should target a stable session/event API later. The first
   deliverable is a rock-solid backend that PR dispatch and tests can drive headlessly.
3. **Config owns knobs.** System prompts, model choices, role definitions, turn budgets, tool policy,
   validation commands, sandbox image/runtime, retry thresholds, and tuning settings belong in config
   files with validated defaults. The binary should not need a recompile to change role behavior.
4. **Small tools beat clever tools.** Prefer opencode-shaped discrete tools (`read_file`, `edit_file`,
   `write_file`, `search`, `run_command`) over vtcode-shaped `unified_file` tools. Simpler schemas
   reduce model ambiguity and make loop progress easier to measure.
5. **Safety is code.** File path containment, command policy, sandboxing, dirty-worktree protection,
   deletion guards, PR-only publication, and proposal/approval boundaries are deterministic code, not
   prompt requests.
6. **Context is budgeted and staged.** The planner sees summaries and catalog-like affordances first;
   large file contents are pulled only when a tool or helper service determines they are relevant.
   The existing tool-helper/context-partitioning direction stays central.
7. **Empiricism is part of the product.** The coder must ship with fixture tests, live smoke tests,
   eval scenarios, and `heuristics-tuner` integration from the beginning, not after dogfooding finds
   regressions.

## Proven Harness Patterns To Steal

The design should shamelessly reuse good ideas from the strongest existing coding harnesses while
keeping Liberado's Rust/modular/config-first constraints.

Research inputs:

- DeepWiki comparison of `sst/opencode` and `langchain-ai/langgraph` for loop state, tool execution,
  sessions/events, checkpointing, provider abstraction, and loop safeguards:
  <https://deepwiki.com/search/what-architecture-patterns-in_ad6db9d1-5ec2-4da3-a409-05ffa20c3191>
- Claude Code Agent SDK docs on the core autonomous loop, tools, permissions, cost limits, hooks,
  and output control:
  <https://code.claude.com/docs/en/agent-sdk/agent-loop>
- Claude Code architecture analysis paper identifying the simple model/tool/result loop plus the
  surrounding systems that make it work: permission modes, context compaction, extensibility,
  subagent/worktree isolation, and append-oriented session storage:
  <https://arxiv.org/abs/2604.14228>
- Local loop-architecture note captured from the user's reference article:
  [`loop_architecture_reference_article.md`](../ideas/loop_architecture_reference_article.md)

Implementation lessons:

- **Session first.** Treat a coding run as a durable session with append-only messages/events, not
  just a function call. `CoderTrace` should become replayable enough for debugging, eval promotion,
  and future TUI rendering.
- **Verifier first.** A loop without an automated verifier is only repetition. Success has to pass
  deterministic gates: real diff, path policy, validation, deletion guard, report/diff consistency,
  and critic review where configured.
- **State compounds.** Every attempt should leave enough state that the next attempt can resume from
  what was tried, what failed, what changed, and why. This belongs in trace/session artifacts, not
  transient model memory.
- **Stop conditions are product behavior.** Every autonomous run must terminate as one of a small
  number of named states: success, no changes, validation failed, budget exhausted, blocked, policy
  denied, needs human review. Silent endless retry is a bug.
- **State checkpoints after meaningful steps.** LangGraph-style checkpointing suggests persisting
  state after tool/action supersteps, not only at the final report. This matters for long PR tasks,
  crashes, and self-improvement loops.
- **Explicit continuation decisions.** The loop should have named terminal states and deterministic
  `should_continue` gates: success, no changes, validation failed, needs human review, budget
  exhausted, policy denied, or blocked.
- **Tool runtime as a hard boundary.** Like opencode/Claude Code, tool names and arguments must be
  validated against a registered catalog before execution; tool failures return in-band where the
  model can adapt, while infrastructure failures abort.
- **Events are product surface.** Tool called/succeeded/failed, file changed, validation result,
  loop guard, model turn, and report events should be first-class API, not log scraping.
- **Permissions and sandboxing are layered.** Combine command/path policy, Docker isolation,
  deletion guards, capability scoping, and human draft-PR approval. No single prompt or permission
  mode is the safety boundary.
- **Context compaction is core architecture.** Large context windows are useful, but the harness
  should still summarize, stage retrieval, and keep raw tool output out of main context unless it is
  needed.
- **Subagents should isolate work.** Planner/coder/critic/repair roles need configurable prompts and
  models, but they also need isolated state/workspace assumptions so one role's churn does not pollute
  the others.
- **Outer-loop improvement is separate authority.** The eventual bilevel loop should read traces,
  evals, and heuristic-tuner output to propose prompt/config/tool-policy changes, but those changes
  remain draft artifacts or PRs unless explicitly approved.

## Target Architecture

### Crate Shape

Start with four small crates, then merge only if actual duplication or friction proves they are too
fine-grained:

| Crate | Purpose |
|---|---|
| `coder-core` | Provider-agnostic contracts: `CoderTask`, `CoderBackend`, `CoderSession`, `CoderEvent`, `CoderReport`, `WorkspaceRef`, `SandboxSpec`, role/config structs, and serialized trace schema. No filesystem mutation. |
| `coder-tools` | The concrete coding `ToolRuntime`: file read/search/write/edit/patch, git status/diff, command execution, validation, and path containment. Depends on sandbox/workspace traits, not PR-dispatch. |
| `coder-agent` | Wires `liberado-executor` to `coder-core` and `coder-tools`: builds role prompts from config, creates the tool catalog, runs the loop, emits events/traces, enforces progress gates. |
| `coder-sandbox` | Workspace/sandbox backends: host-local implementation for tests/dev, Docker implementation first for real runs, later remote runners. This crate owns sandbox lifecycle, volume mounts, command adapters, and cleanup. |

`liberado-pr-dispatch-mcp` should become a consumer of those crates. It can be collapsed into the main
workspace or renamed once the backend boundary exists. The likely destination is a core PR-factory
crate plus an MCP/server adapter:

| Future crate/surface | Purpose |
|---|---|
| `pr-factory-core` | Task DB, forge abstraction, clone/branch/commit/push/PR lifecycle, revision loop, deletion guard, validation result handling. |
| `pr-factory-mcp` or `code-dispatch-mcp` | MCP/HTTP transport exposing submit/status/approve/revise/query tools. Thin adapter over `pr-factory-core`. |

### Backend Trait

The migration seam should be a backend trait around "make code changes in this prepared workspace":

```rust
#[async_trait]
pub trait CoderBackend: Send + Sync {
    async fn run(&self, request: CoderRunRequest) -> Result<CoderRunResult, CoderError>;
}
```

`VtcodeBackend` wraps the existing client during migration. `LiberadoLoopBackend` uses
`coder-agent`. `pr-factory-core` should only know the trait, not which implementation is active.

### Coding Tool Surface

The v1 tool catalog should be intentionally boring:

| Tool | Notes |
|---|---|
| `list_files` | Glob-aware, capped output, respects ignores and workspace root. |
| `search_text` | `rg`-style lexical search; output capped and structured. |
| `read_file` | Requires path and optional range; hard byte/line caps. |
| `write_file` | Full-file write for new/small files; gated by path policy. |
| `edit_file` | Exact old/new replacement; returns failure with context if old text is absent or ambiguous. |
| `apply_patch` | Atomic multi-file exact replacements; deterministic validation before any mutation. |
| `git_status` | Porcelain plus ignored/scratch filtering. |
| `git_diff` | Name-only/stat/patch modes with caps. |
| `run_command` | Policy-checked command execution inside sandbox. |
| `validate` | Runs configured validation commands and returns structured pass/fail output. |
| `submit_report` | Existing executor finish tool, backed by `Report`/`CoderReport`. |
| `scratchpad` | Existing in-process scratchpad, not a real workspace tool and not loop-detected as a repeated action. |

The engine should treat `scratchpad` and reporting tools specially, exactly as
`liberado-executor` does today.

### Role Model

Roles are config-defined. The binary can provide typed role slots and defaults, but the actual prompts
and model choices come from config:

| Role | Default intent |
|---|---|
| `planner` | DeepSeek V4 Pro by default. Reads task/repo context, proposes plan and expected files. May be skipped for simple tasks. |
| `coder` | DeepSeek V4 Pro or configured coding model. Executes edits in the workspace loop. |
| `critic` | DeepSeek V4 Flash by default. Reviews actual `git diff` against task requirements. |
| `repair` | Usually same as coder or Flash for cheap validation-failure repair, config-selected. |
| `summarizer` | Condenses traces, file reads, and task state to reduce context pollution. |
| `tool_advisor` | Optional: delegates tool/file-selection reasoning away from the main helper model, aligned with the tool-helper MCP direction. |

Config must support per-role provider/model, temperature, max tokens, prompt path or inline prompt,
turn budget, tool visibility, and stop/progress thresholds. The defaults should prefer
`deepseek/deepseek-v4-pro` for planner/coder and `deepseek/deepseek-v4-flash` for cheaper helper
roles, but no code should assume those strings.

### Sandbox Model

Design for sandboxing on day one even if host-local remains useful for tests.

Required abstractions:

- `WorkspaceBackend`: prepare, snapshot, diff, cleanup.
- `CommandRunner`: execute policy-checked commands in the workspace.
- `SandboxBackend`: host-local, Docker, later remote/Firecracker/CI runner.
- `PathPolicy`: root containment, denied globs, allowed write roots, generated scratch paths.
- `CommandPolicy`: allowlist/denylist, network policy, timeout, output cap, environment allowlist.

Docker should be the first concrete production sandbox. It should support configured images, volumes,
network mode, env allowlist, user id, workdir, and cleanup. The existing Docker MCP transport in
[`phase-4-docker-transport.md`](phase-4-docker-transport.md) proves the repo already accepts Docker as
an isolation primitive; coder sandboxing is a different layer, but should use the same config style.

### Event and Session API

Do not build a bespoke coding TUI now. Build an event stream good enough for one later.

`coder-core` should define a stable event vocabulary:

- `session_started`
- `role_started` / `role_finished`
- `model_turn_started` / `model_turn_finished`
- `tool_started` / `tool_finished`
- `file_changed`
- `command_started` / `command_finished`
- `validation_started` / `validation_finished`
- `progress_checkpoint`
- `loop_guard_triggered`
- `critic_verdict`
- `report_filed`
- `session_finished`

PR dispatch consumes these for logs and diagnostics. A future TUI/CLI can render them as a live coding
session, like Claude Code CLI, without owning the loop.

## Loop Shape

The production loop should be explicit and traceable:

1. **Perceive**: task, repo metadata, branch, existing diff, config, policy, previous attempts,
   validation history, relevant repo docs.
2. **Plan**: optional planner pass for complex tasks. Produces plan, likely files, commands, and
   success criteria. Can be bypassed by config or classifier for narrow tasks.
3. **Act**: coder loop uses the small coding tool catalog to inspect, edit, run commands, and validate.
4. **Observe**: tool results, diff state, validation result, progress metrics, loop guards.
5. **Repair/replan**: validation failures, no-diff outcomes, critic findings, or progress stalls produce
   specific next instructions, not blind retries.
6. **Report**: final structured report is accepted only if deterministic gates agree with it.
7. **PR factory**: commit, deletion guard, push, draft PR, notify, revise/approve loop.

Important: the model's `submit_report` cannot be trusted by itself for success. The engine must check:

- Is there a non-empty real diff?
- Did edits touch allowed paths only?
- Did deletion guard pass?
- Did configured validation pass or produce an accepted failure state?
- Did the critic accept, or are critic concerns attached to the draft PR?
- Did the final report's file list match `git diff --name-only` closely enough?

## Progress and Loop Guards

Generic doom-loop detection already exists in `liberado-executor`; coding adds domain-specific gates:

- **Read-only stall**: after N model turns or M tool calls with no diff, inject a targeted nudge; after
  another threshold, replan or fail with `NoChanges`.
- **Same-file churn**: repeated reads/edits of the same file without changing diff status.
- **Search churn**: repeated equivalent searches with no new files selected.
- **Validation churn**: same validation failure repeated after an attempted repair.
- **Diff regression**: large deletion or unrelated-file expansion after a repair pass.
- **Report mismatch**: model claims success while `git status`/validation contradicts it.

Every threshold is config-owned and logged. No magic numbers should be introduced directly into the
loop implementation without a config type and a documented default.

## Config Surface

Add a `[coder]` section in `tuning.toml` for the existing config stack. Ownership is now clear:
`liberado-config-loader::CoderTuning` validates and converts to `CoderRunConfig`.

Likely sections:

```toml
[coder]
backend = "liberado-loop" # or "vtcode" during migration
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

The example above is illustrative, not final. The implementation should validate all referenced
prompt paths, model roles, sandbox backends, command policies, and unknown fields before serving.

## Migration Plan

### Phase 0: Documentation and Boundary

- Write this plan and link it from architecture/roadmap/handoff docs.
- Record that `vtcode` is now a migration backend, not the strategic endpoint.
- In `liberado-pr-dispatch-mcp`, introduce a `CoderBackend` seam around the existing `VTCodeClient`
  execution path without changing behavior.
- Add an adapter result type that carries summary, files changed, diagnostics, trace path, and engine
  name.

### Phase 1: Core Contracts

- Add `coder-core`.
- Define `CoderTask`, `CoderRunRequest`, `CoderRunResult`, `CoderBackend`, `CoderEvent`,
  `CoderTrace`, `SandboxSpec`, `CoderCommandConfig`, `CommandPolicy`, and config structs.
- Add serialization tests and examples.
- Add architecture doc for the crate.
- Expose coder settings through the main config stack. **Started with `CoderTuning` in
  `liberado-config-loader`, re-exported by `liberado-config`, plus `config.example/tuning.toml`.**

### Phase 2: Tool Runtime and Host Sandbox

- Add `coder-tools` and host-local `coder-sandbox` implementation for tests/dev. **Started.**
- Implement discrete file/git/search/command/validation tools behind `ToolRuntime`. **Started with
  `list_files`, `search_text`, `read_file`, `write_file`, `edit_file`, `git_status`, `git_diff`,
  `apply_patch`, `run_command`, and `validate`.**
- Enforce root path containment and output caps. **Started.**
- Honor configured sandbox backend in the tool runtime. **Started with host-local and Docker
  workspace selection via `CodingToolRuntime::from_sandbox`; live Docker task smoke remains.**
- Add unit tests for path escapes, ambiguous edits, patch failures, command denial, and output caps.
  **Path escapes, command denial, ambiguous edit, atomic patch failure, basic read/write/search, and
  read output caps are covered.**

### Phase 3: Liberado Loop Backend MVP

- Add `coder-agent`. **Started.**
- Wire `liberado-executor::Executor` to the coding tools with config-loaded prompts. **Started for
  inline `config.coder.prompt` and resolved `config.coder.prompt_path`.**
- Implement no-diff progress detection and report verification. **Started with deterministic
  post-loop `git status --porcelain` and configured validation-command gating; in-loop progress
  guards remain.**
- Produce structured `CoderEvent` traces. **Started with session/role/report/tool-start/tool-finish/
  file-change/validation/guard/finish events persisted as `CoderTrace`; model-turn events remain.**
- Add a process boundary for callers that cannot or should not link the loop stack directly.
  **Started with `liberado-coder-run`, which accepts `CoderRunRequest` JSON and emits
  `CoderRunResult` JSON.**
- Add per-role provider selection so planner/coder/critic/repair can use different configured
  models. **Started with `CoderProviderFactory`; only the coder role consumes it today.**
- Run against a mocked provider first, then a small live smoke task. **Mocked provider tests are
  covered; an ignored OpenRouter/DeepSeek live smoke exists for manual runs.**

### Phase 4: PR Factory Integration

- Collapse or restructure `liberado-pr-dispatch-mcp` as needed.
- Let config select `vtcode` or `liberado-loop`. **Started with `CODING_BACKEND`; `liberado-loop`
  spawns `liberado-coder-run` and maps `CoderRunResult` back into the existing worker contract.
  `LIBERADO_CODER_RUN_CONFIG_PATH` can now point PR dispatch at a full `CoderRunConfig` JSON file so
  sandbox/tool/progress/prompt knobs do not have to be duplicated in the nested crate.**
- Keep PR lifecycle behavior unchanged: draft PR only, human approval, revision loop.
- Preserve useful existing modules: forge client, DB, branch validation, GIT_ASKPASS, deletion guard,
  validation/self-correction, repo-context injection, critic loop.
- Add an e2e test where the mocked Liberado loop backend edits files and opens a draft PR.

### Phase 5: Docker Sandbox

- Implement Docker `SandboxBackend`. **Started with `DockerWorkspace`, policy-checked Docker argv
  construction, configured workspace bind mount, volumes, network mode, user, env allowlist, and
  command execution. Long-lived lifecycle/image management is not built yet.**
- Add config validation and examples. **Config validation exists for blank Docker image in the main
  config model; coder-specific Docker examples still need to be added once the agent selects sandbox
  backends from `CoderRunConfig`.**
- Run a live end-to-end smoke test in a container.
- Ensure no host filesystem access outside mounted workspace and configured cache dirs.

### Phase 6: Empirical Evaluation

- Add `coder-eval` scenarios or extend `heuristics-tuner` with a `Coder` layer.
- Scenario classes:
  - narrow one-file edit;
  - multi-file feature;
  - test failure repair;
  - no-op/ambiguous task should clarify or fail cleanly;
  - deletion guard task;
  - repeated read-only model must be redirected or fail;
  - revision task against an existing branch.
- Metrics:
  - success rate;
  - non-empty diff rate;
  - validation pass rate;
  - unrelated-file touch rate;
  - retry count;
  - token and wall-clock cost;
  - unsafe/disallowed command attempts;
  - no-progress loop detections.
- Feed results into `liberado-heuristics-tuner` so prompt/model/policy changes are proposed with
  evidence, not intuition.

### Phase 7: TUI/CLI Readiness

- Expose `CoderSession` event stream over the existing daemon/server style API.
- Teach the TUI to render coder sessions as a client only. It should not own planning, tool execution,
  sandbox lifecycle, or PR state.
- Add session replay from trace files so failures can be inspected without rerunning the model.

### Phase 8: Self-Improvement Loop

- Let the coder operate on Liberado itself through the PR factory.
- Heuristics tuner generates candidate prompt/config changes as draft PRs or proposal artifacts.
- Failed/interesting live traces can be promoted into eval fixtures.
- Architecture-critique mode writes idea docs, not code, unless separately dispatched through the PR
  factory and draft-PR gate.
- The system never silently widens its own authority. Config changes remain human-owned unless a future
  explicit proposal/approval mechanism is designed for them.

## Reuse From PR Dispatch

Keep:

- `forge-client` abstraction and clone credentials hygiene.
- SQLite task lifecycle and dependency handling.
- branch validation before queueing work.
- draft PR creation and publish/revise/reject flow.
- Telegram/webhook notification behavior.
- repo-context injection from `AGENT_CONTEXT_PATHS`.
- validation command and self-correction loop, generalized away from `VTCodeClient`.
- critic loop reviewing actual `git diff`.
- no-changes detection and vtcode diagnostic lessons.

Replace:

- direct dependence on `VTCodeClient` as the only coding engine;
- vtcode-specific config/session assumptions;
- vtcode event parsing as the canonical trace schema;
- prompts that mention vtcode/unified tools once the Liberado loop backend is active.

## Documentation Work Required

- Add `ARCHITECTURE.md` for each new crate as it lands.
- Update [`docs/architecture/overview.md`](../architecture/overview.md) when the crate map changes.
- Keep [`docs/roadmap/current.md`](current.md) as the high-level state pointer.
- Update `liberado-pr-dispatch-mcp/ARCHITECTURE.md` or its replacement when the backend boundary lands.
- Keep prompt/config examples in `config.example/`, not in source constants.
- Add "how to run coder evals" to the relevant contributing doc once the evaluator exists.

## Open Design Questions

- Should `coder-core` live in the main workspace before or after `liberado-pr-dispatch-mcp` is
  collapsed into it?
- Does `coder-agent` use `Report` directly, or a richer `CoderReport` that converts into `Report` at
  mesh boundaries?
- Should planner output be a typed JSON plan consumed by code, or an advisory artifact visible to the
  coder role? Lean typed but non-binding: deterministic code validates, the coder can deviate with
  traceable rationale.
- How much command allowlisting should be global vs repo-specific? Lean repo-specific overlay on top
  of global defaults.
- Should Docker sandbox images be per-language, per-repo, or a general image with caches mounted?
  Lean general image first, repo override later.
- How should large-context models be used without encouraging context dumping? Lean staged retrieval
  plus summaries, even with 1M context available.

## First Concrete Slice

The first implementation slice should be deliberately small:

1. Add `CoderBackend` to PR dispatch and wrap current vtcode behavior.
2. Add `coder-core` with contracts and config structs.
3. Add a fake/mock `LiberadoLoopBackend` test implementation that produces a diff through the same PR
   pipeline.
4. Commit docs and tests.

Only then add real file tools and model calls. This keeps the migration reversible and proves the PR
factory can stop caring which coding engine produced the workspace diff.
