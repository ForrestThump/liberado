# liberado-coder-agent — coding domain pack (goal session)

**Role in the system:** this crate is the **coding domain pack's** goal-session composition — not
Liberado's orchestration kernel. Shared kernel pieces are `liberado-executor`, `liberado-provider`,
`liberado-common`, and (later) domain-neutral session types. See
[`docs/spec/architecture/agentic-loops.md`](../../docs/spec/architecture/agentic-loops.md) and
[`docs/future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md`](../../docs/future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md).

Implements `CoderBackend`: multi-attempt outer loop over `Executor`, coding tools, deterministic
gates, progress guards, optional critic. UIs and the PR factory must not reimplement this loop.

## Module map (decomposition)

| Module | Responsibility |
|---|---|
| `lib` | `LiberadoLoopBackend`, provider factory, attempt loop composition |
| `roles` | Worker/repair selection, prompt load, goal text |
| `gates` | Backend git status, legacy validation helper, progress-fatal mapping |
| `verify_pipeline` | Config-driven completeness/CI gates (`paths_exist`, `content_contains`, `command`, …) |
| `intake_session` | Criteria intake (`run_intake` → `IntakeOutcome` → freeze → `apply_to_request`) |
| `planner` | Optional structured plan before worker (config-skippable) |
| `repair_feedback` | Failure-class / signature formatting for repair attempts |
| `critic` | Model critic on real git diff (maker ≠ checker) |
| `progress` | In-loop read-only / same-tool / validation-churn guards |
| `runtime` | `ToolRuntime` wrapper: tracing + progress |
| `trace` | Event log + optional `CoderTrace` artifacts |
| `fanout` | **S6:** parallel coding subagents on named worktree branches + parent LLM merge-back |
| `session_pack` | Goal-session adapter; routes `payload.subtasks` into `fanout` |

## Behavior

- Optional **planner** (when `planner.prompt` / `prompt_path` set): structured plan → task context before worker (attempt 0 only).
- Worker role: `coder` (attempt 0) or `repair` (later attempts when configured).
- Requires resolved `max_turns` on the worker role.
- `SandboxSpec` → `CodingToolRuntime` (host or Docker).
- Post-loop: `git status` no-diff gate; verifier pipeline / optional validation command.
- **Failure-signature repair**: validation failures carry `FAILURE_CLASS` + `FAILURE_SIGNATURE` + hints; repair goal prioritizes the latest signature.
- Critic: opt-in when `critic.prompt` / `prompt_path` set; reviews evidence, may force retry.
- Progress guards: config thresholds from `ProgressPolicy`.
- Attempts: `max_attempts` with signature-aware `prior_feedback` on NoChanges / validation / critic revision.
- Traces when `trace_dir` set.

## Dependency posture

Depends on foundation (`executor`, `provider`, `common`) + coding pack (`coder-core`, `coder-tools`,
`coder-sandbox`). Must not become a dependency of non-coding domains. Patterns that prove general
(attempt loop, progress policy shape, session events) graduate upward per modularity extraction
triggers — they do not pull life-ops into this crate.

## Coding fan-out (S6)

When a coding goal carries `payload.subtasks` (array of `{label, description, success_criteria?}`):

1. Each subtask gets `git worktree add -b fanout/<label>-i` under `coding-worktrees/`.
2. **Production (hub attached):** each child is a **background coding goal session**
   (`start_background` + `await_terminal`), grant = parent without `AskHuman`,
   `payload.fanout_child` + `force_host_local` (no nested worktree). Nested `subtasks` refused.
3. **Fallback (tests / no hub):** in-process `CoderBackend` workers on the same worktrees.
4. Concurrency semaphore: `payload.max_concurrent_subagents` → overrides → pack field from
   `tuning.dispatch.max_concurrent_coding_subagents` (**default 3**).
5. Worktrees removed after each child; **branches remain**. Parent merges (`--no-ff`); conflicts
   go through merge-role LLM. Children **never** self-merge.

Helpers: `coder-sandbox` `merge` module; orchestration in `fanout.rs`. Pack gets hub via
`CodingSessionPack::attach_hub` after server `Arc`s the hub.

Open product work for this crate lives in [`docs/future-work/backlog.md`](../../docs/future-work/backlog.md), not here.

## Tests (escalation ladder)

Stay on lower rungs until they are green. Do not jump to full live coding first.

| Rung | Command | Network / key? |
|---|---|---|
| **0 – full mock e2e** | `cargo test -p liberado-coder-agent --test mock_intake_e2e` | No |
| **unit mocks** | `cargo test -p liberado-coder-agent --lib` | No |
| **1 – live intake only** | `cargo test -p liberado-coder-agent --test live_scaffold live_intake_schema_smoke -- --ignored --nocapture` | `OPENROUTER_API_KEY` |
| **2 – live intake + mock worker** | `cargo test -p liberado-coder-agent --test live_scaffold live_intake_then_mock_worker -- --ignored --nocapture` | key; worker mocked |
| **3 – live coding smoke** | `cargo test -p liberado-coder-agent openrouter_deepseek_live_coding_smoke -- --ignored` | key + real worker |

Fixtures: `tests/fixtures/intake_*.json` (structural gates only — no `cargo` profiles so rung 0 stays CI-safe). Shared helpers: `tests/common/mod.rs`.
