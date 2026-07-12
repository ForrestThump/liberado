# Agentic Loops — Architecture

**Status**: living architecture, 2026-07-10  
**Implementation roadmap**: [`docs/roadmap/rust-native-agentic-coder-plan.md`](../roadmap/rust-native-agentic-coder-plan.md)  
**Hygiene audit**: [`docs/roadmap/agentic-mesh-hygiene-audit-2026-07-10.md`](../roadmap/agentic-mesh-hygiene-audit-2026-07-10.md)  
**Modularity**: [`modularity.md`](modularity.md) · **Mesh vision**: [`meshify.md`](../ideas/meshify.md)

Design inputs:
[`loop_architecture_reference_article.md`](../ideas/loop_architecture_reference_article.md),
[`doomloop_research.md`](../ideas/doomloop_research.md),
Claude Code / Codex / OpenCode / Grok Build harness patterns, Liberado's
`Provider` + `Executor` + `ToolRuntime` substrate.

---

## Product intent (read this first)

Liberado is building a **general, mesh-like agentic orchestration framework** in Rust — not a
coding-only product that happens to use agents.

| | |
|---|---|
| **Kernel** | Goal sessions, budgets, terminal states, role graph, subagent delegation, verifiers-as-code, durable attempt state, session/event API |
| **Domain packs** | Pluggable limbs: tools (`ToolRuntime`), env/sandbox, progress metric, domain verifiers, role prompts |
| **Surfaces** | TUI, WebUI, CLI, PR-factory MCP, cron/hooks — **clients only**; they never own the loop |
| **First domain pack** | **Coding** (`coder-*`) — unblocks PR-dispatch self-improvement and proves the harness |
| **Other domains** | Life-ops MCP mesh, research, custom tools — same kernel, different packs |

Coding is first because reliability there is the self-improvement gate. **It is not the center of
gravity.** If a design only makes sense for git/diff/cargo, it belongs in the coding pack, not the
kernel.

**Not a VTCode wrap.** Coding pack is Liberado-owned (`coder-*` + `executor`). VTCode is a failed
external harness / temporary cutover only.

**Verifiers (CI-in-the-loop):** harness-owned success checks — config-listed commands, path/content
gates, repair-until-green-or-budget — are sketched in [`verifiers.md`](verifiers.md). Kernel stays
language-agnostic; coding is the first profile.

---

## Mesh placement

Agentic loops **compose** existing Liberado pieces; they do not replace the mesh.

```
Surfaces (clients — no loop ownership)
  TUI · WebUI · CLI · PR-factory MCP · cron/hooks · future ACP
        │
        │  session API + append-only event stream  (domain-neutral envelope, long-term)
        ▼
┌───────────────────────────────────────────────────────────────┐
│  GOAL SESSION KERNEL  (orchestration — domain-agnostic)       │
│  Goal · criteria · roles · subagents · budgets · terminals    │
│  attempt log · progress policy hook · verifier ports          │
└────────────────────────────┬──────────────────────────────────┘
                             │ N bounded Executor runs
                             ▼
              liberado-executor  (inner tool loop — already general)
                             │
          ┌──────────────────┼──────────────────┐
          ▼                  ▼                  ▼
   Coding pack         MCP / life-ops      Future packs
   coder-tools         TurbomcpRuntime     research, ops, …
   coder-sandbox       capability zones
   git/validate gates  vault verifiers
```

| Existing piece | Mesh role |
|---|---|
| `Provider` | Inference only — no control flow |
| `Executor` | **Inner loop** — tools + budget + generic doom-loop |
| `ToolRuntime` | **Domain limb** — the primary extension point |
| `Dispatcher` / `Orchestrator` | Route goals; subagent dispatch with capability ∩ |
| `main-agent` / `ChatSessions` | Human-in-the-chair converse mode (not goal-until-done) |
| `coder-*` | **Coding domain pack** (specialization, not kernel) |
| `heuristics-tuner` | Meta-loop seed (draft proposals only) |
| `Report` / `Outcome` / `CapabilitySet` | Shared mesh boundary types |

**Reusability test**: a non-coding goal (“update vault note + mark task done”) with MCP tools,
automated criteria, turn budget, and critic-on-evidence must reuse the **same kernel** without
depending on git, `coder-tools`, or workspace sandbox.

---

## Kernel vs domain pack (stable seams)

Design seams **now**. Extract crates when a second domain would otherwise copy types — not before
(see audit). Logical contracts:

| Seam | Kernel owns | Domain pack owns |
|---|---|---|
| **Goal** | id, description, success_criteria, budget, parent_id | domain tag, pack-specific constraints |
| **Terminal states** | Succeeded, Blocked, BudgetExhausted, PolicyDenied, Failed, NeedsHumanReview, … | domain mapping (e.g. coding `NoChanges`, `ValidationFailed`) |
| **Verifier port** | when to run gates; fail-closed order | structural/process checks (diff, tests, vault fields, HTTP) — see [`verifiers.md`](verifiers.md) |
| **Progress metric** | stall policy (nudge → fatal) | “did progress move?” (mutation/diff hash vs artifact change) |
| **Tool limb** | `ToolRuntime` trait only | catalog + invoke + policy |
| **Role graph** | planner / worker / critic / repair slots | prompts, models, tool visibility per domain |
| **Session events** | envelope: session/role/tool/guard/verifier/finish | domain payloads (file_changed, etc.) |
| **Subagents** | child goal, capability ∩, budget, Report return | isolation (worktree vs none), tool narrowing |
| **Surfaces** | session API shape | optional domain renderers |

### Coding pack today (`coder-*`)

| Crate | Pack responsibility |
|---|---|
| `coder-core` | Coding contracts (`CoderTask`, `CoderEvent`, …) — **specialization** of Goal/Session vocabulary |
| `coder-sandbox` | Workspace / command isolation |
| `coder-tools` | Coding `ToolRuntime` |
| `coder-agent` | Coding composition: attempt loop, progress, critic, gates over executor |
| `coder-runner` | Process adapter for nested consumers (PR factory) |

`coder-agent` is **not** the Liberado kernel. It is the first *implementation* of a goal session for
one domain. Patterns proven here graduate upward (common types or a future `session` crate).

### Dependency rules (loose coupling)

1. Domain packs depend on kernel/mesh types (`common`, `provider`, `executor`) — not on other packs.
2. Surfaces depend on session contracts — not on domain tool implementations.
3. PR factory / forge lifecycle is a **consumer**, not part of the coding loop.
4. Kernel must not import git, cargo, or path sandbox types.
5. Config: kernel knobs (budgets, terminals) vs pack knobs (`[coder]`, MCP grants) stay separate.

> **Known violation (2026-07-11 audit):** `config-loader` → `coder-core` is a real dependency
> (verify/intake DTOs for the `[coder]` section), so the whole config stack sits on the coding
> pack. Fix = lift neutral verify types into `common`/`liberado-verify`
> ([modularity.md](modularity.md) extraction-trigger status note), not a pack import in config.

---

## What a loop is

> Define a goal once. The system plans when useful, acts with tools, verifies against something the
> model does not control, records durable state, and continues, escalates, or stops.

| Piece | Without it |
|---|---|
| **Verifier** | Agent grades its own homework |
| **State** | Same mistake every cycle; no resume |
| **Stop conditions** | Infinite spend or silent stall |

Doom loops are a **control-flow** problem. Guards live in the harness. See
[`doomloop_research.md`](../ideas/doomloop_research.md).

### Two loop levels

**Inner** (`liberado-executor`): one role, one `ToolRuntime`, budget, report or converse termination.
Generic doom-loop / short-cycle detection lives here.

**Outer (goal session)**: multi-role graph, domain verifiers, progress metric, attempt log, named
terminals. Coding pack implements this in `coder-agent` today.

**Meta**: `heuristics-tuner` + draft PRs — improve process from evidence; never silent self-widening
of authority (Decision 14).

### Maker ≠ checker

Worker produces; **deterministic verifiers** then optional **critic role** on evidence (diff, logs,
artifacts). Never critic alone for success.

---

## Terminal states (kernel vocabulary)

Map to `liberado_common::Outcome` at mesh boundaries.

| State | Meaning |
|---|---|
| `Succeeded` | Verifiers pass; criteria met |
| `NoProgress` / coding `NoChanges` | Claimed success with no domain effect |
| `ValidationFailed` | Process gate failed after repair budget |
| `Blocked` | Missing info, env, or dependency |
| `NeedsHumanReview` | Policy / critic / PR gate |
| `BudgetExhausted` | Turns, time, tokens, attempts |
| `PolicyDenied` | Capability / path / command refusal |
| `Failed` | Hard error |

---

## Drift control (kernel)

1. Re-anchor goal + criteria + last verifier failure each outer iteration  
2. Progress metrics owned by code (domain supplies the metric)  
3. Read-only / no-progress stalls → nudge → fatal terminal  
4. Failure-signature memory (validation churn)  
5. Report consistency vs structural evidence  
6. Scratchpad + durable attempt log outside the scrolling chat window  
7. Context compaction / staged retrieval (dispatcher-as-tool-advisor pillar)

---

## Surfaces

| May | Must not |
|---|---|
| Render events, budgets, diffs/artifacts | Own tool execution or sandbox lifecycle |
| Send goals, approvals, cancel | Bypass capability / draft-PR gates |
| Replay traces | Reimplement planner/critic loops |

Headless first (PR dispatch, evals). TUI/WebUI target one session/event API later — converge
`AgentEvent` (chat) and `CoderEvent` (coding) into a shared envelope with domain payloads
(see hygiene audit).

---

## Implementation status

| Layer | Status |
|---|---|
| Inner loop (`executor`) | Production |
| Coding pack tools + sandbox | Landed |
| Coding goal session (`coder-agent`) | Worker + optional planner + repair (signature routing) + critic + progress guards + gates + traces |
| Neutral Goal/Session types | **`liberado-session` crate** (GoalSpec, SessionEvent, hub) |
| Non-coding domain pack proof | **LifeOpsDemoRunner** (no coder-tools) |
| Unified session API for TUI/WebUI | **HTTP/SSE** `/api/goals*` on `liberado-server` |
| Meta-loop | Tuner + draft proposal export (Decision 14) |

Module layout in `coder-agent` (composition, not kernel): `roles`, `gates`, `critic`, `progress`,
`runtime`, `trace`, `lib` (backend orchestration only).

---

## Patterns stolen deliberately

Claude Code, Codex, OpenCode, Grok Build-class TUIs, Karpathy/bilevel loops, MAST/doom-loop research
— **harness patterns**, not framework imports. Liberado keeps the Rust modular mesh.

---

## Design rules

1. **Mesh first, domain packs second** — coding is a pack, not the product identity.  
2. **Verifier before vibes.**  
3. **State compounds; named terminals only.**  
4. **Maker ≠ checker; intake ≠ worker.** Criteria come from human/profile/**intake freeze**, not the worker.  
5. **Small tools; config owns knobs.**  
6. **UI is a client.**  
7. **Design neutral seams now; extract crates when friction is real.**  
8. **Meta-loop proposes; humans dispose.**  
9. **Could someone use just this crate?** — modularity test applies to packs and kernel.  
10. **Second-domain test is the pigeonhole detector** — if only coding can use it, it is not kernel.

Criteria intake and frozen verifier contracts: [`verifiers.md`](verifiers.md) §3.
