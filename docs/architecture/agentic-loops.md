# Agentic Loops — Architecture

**Status**: living architecture, 2026-07-10  
**Implementation roadmap**: [`docs/roadmap/rust-native-agentic-coder-plan.md`](../roadmap/rust-native-agentic-coder-plan.md)  
**Hygiene audit**: [`docs/roadmap/archive/agentic-mesh-hygiene-audit-2026-07-10.md`](../roadmap/archive/agentic-mesh-hygiene-audit-2026-07-10.md)  
**Modularity**: [`modularity.md`](modularity.md) · **Contracts**: [`contracts.md`](contracts.md) ·
**Event-bus idea (annotated, partly superseded)**: [`meshify.md`](../ideas/archive/meshify.md)  
**Channels & interactivity** (authority vs information graph; interactivity as a capability, not a subtype): [`channels-and-interactivity.md`](channels-and-interactivity.md)

Design inputs:
[`loop_architecture_reference_article.md`](../ideas/archive/loop_architecture_reference_article.md),
[`doomloop_research.md`](../ideas/archive/doomloop_research.md),
Claude Code / Codex / OpenCode / Grok Build harness patterns, Liberado's
`Provider` + `Executor` + `ToolRuntime` substrate.

---

## Product intent (read this first)

Liberado is building a **general agentic orchestration framework** in Rust — kernel + domain packs
+ stores + surfaces — not a coding-only product that happens to use agents.

| | |
|---|---|
| **Kernel** | Goal sessions, budgets, terminal states, role graph, subagent delegation, verifiers-as-code, durable attempt state, session/event API |
| **Domain packs** | Pluggable limbs: tools (`ToolRuntime`), env/sandbox, progress metric, domain verifiers, role prompts |
| **Surfaces** | TUI, WebUI, CLI, PR-factory MCP, cron/hooks — **clients only**; they never own the loop |
| **First domain pack** | **Coding** (`coder-*`) — unblocks PR-dispatch self-improvement and proves the harness |
| **Other domains** | Life-ops MCPs, research, custom tools — same kernel, different packs |

Coding is first because reliability there is the self-improvement gate. **It is not the center of
gravity.** If a design only makes sense for git/diff/cargo, it belongs in the coding pack, not the
kernel.

**Not a VTCode wrap.** Coding pack is Liberado-owned (`coder-*` + `executor`). VTCode is a failed
external harness / temporary cutover only.

**Verifiers (CI-in-the-loop):** harness-owned success checks — config-listed commands, path/content
gates, repair-until-green-or-budget — are sketched in [`verifiers.md`](verifiers.md). Kernel stays
language-agnostic; coding is the first profile.

---

## Kernel placement

The loop hierarchy **composes** existing Liberado pieces; it does not replace the existing kernel.

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
              liberado-executor  (turn loop — already general)
                             │
          ┌──────────────────┼──────────────────┐
          ▼                  ▼                  ▼
   Coding pack         MCP / life-ops      Future packs
   coder-tools         TurbomcpRuntime     research, ops, …
   coder-sandbox       capability zones
   git/validate gates  vault verifiers
```

| Existing piece | Kernel role |
|---|---|
| `Provider` | Inference only — no control flow |
| `Executor` | **Turn loop** — tools + budget + generic doom-loop |
| `ToolRuntime` | **Domain limb** — the primary extension point |
| `Dispatcher` / `Orchestrator` | Route goals; subagent dispatch with capability ∩ |
| `main-agent` / `ChatSessions` | Human-in-the-chair converse mode (not goal-until-done) |
| `coder-*` | **Coding domain pack** (specialization, not kernel) |
| `heuristics-tuner` | Meta-loop seed (draft proposals only) |
| `Report` / `Outcome` / `CapabilitySet` | Shared kernel boundary types |

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

1. Domain packs depend on kernel types (`common`, `provider`, `executor`, `session`) — not on other packs.
2. Surfaces depend on session contracts — not on domain tool implementations.
3. PR factory / forge lifecycle is a **consumer**, not part of the coding loop.
4. Kernel must not import git, cargo, or path sandbox types.
5. Config: kernel knobs (budgets, terminals) vs pack knobs (`[coder]`, MCP grants) stay separate.

> **Resolved (2026-07-11, same-day):** `config-loader` → `coder-core` was a real dependency (the
> `[coder]` tuning vocabulary). Fixed by inversion: config-loader carries `[tuning.coder]` as an
> opaque `toml::Value`; `CoderTuning::from_value` (now in `coder-core`) parses + validates at
> composition time. Rule 5 now has teeth: pack config sections are *opaque* to the config stack.

---

## Vocabulary: turn loop · goal · loop · meta-loop

**Fixed 2026-07-12.** "Loop" used to cover three different things in these docs; each now has one
name. The community/harness convention (`/goal` = success-based, `/loop` = time-based) is adopted
as-is.

| Term | Shape | Terminates when | Lives in |
|---|---|---|---|
| **Turn loop** | model → tool → observation → model, bounded, doom-loop-guarded | report filed / prose answer / budget | `liberado-executor` (the inner engine — "loop" is correct and stays; this is what every harness calls the agent loop) |
| **Goal** | success-based: act → verify → repair, until a **terminal state** | verifiers pass, or Blocked / BudgetExhausted / … | goal-session kernel (`liberado-session`) + packs; each attempt runs turn loops |
| **Loop** | time-based recurrence: wake on schedule → inspect state → one improvement pass → log → sleep | **never succeeds closed** — stops on cap, checker-satisfied-N-times, or human close | designed, not yet built — [`loops-plan.md`](../roadmap/loops-plan.md). A loop's body **is a goal**; the loop is a scheduler + series state, not a fourth engine |
| **Meta-loop** | evidence → proposed prompt/config diff → human review | never auto-merges (Decision 14) | `heuristics-tuner` |

Hierarchy: turn loop ⊂ goal ⊂ loop ⊂ meta-loop. Unrelated senses that keep their names:
**doom loop** (a turn-loop failure mode) and **loop-break** (Decision-5 provenance suppression —
and the reason artifact-editing *loops* are safe here: a loop's own writes never re-trigger the
watch pipeline).

### What a goal is

> Define it once. The system plans when useful, acts with tools, verifies against something the
> model does not control, records durable state, and continues, escalates, or stops.

| Piece | Without it |
|---|---|
| **Verifier** | Agent grades its own homework |
| **State** | Same mistake every cycle; no resume |
| **Stop conditions** | Infinite spend or silent stall |

Doom loops are a **control-flow** problem. Guards live in the harness. See
[`doomloop_research.md`](../ideas/archive/doomloop_research.md).

### Maker ≠ checker

Worker produces; **deterministic verifiers** then optional **critic role** on evidence (diff, logs,
artifacts). Never critic alone for success.

---

## Terminal states (kernel vocabulary)

Map to `liberado_common::Outcome` at kernel boundaries.

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

Headless first (PR dispatch, evals). **Event vocabulary converged (2026-07-11):** chat and goal
sessions speak one wire language — `SessionEventKind` (kernel: `liberado-session`; wire mirror:
`chat-client-contract`), one `from_sse_data` decoder for both streams. `AgentEvent` remains the
executor's in-process turn-loop tap, mapped at the server boundary exactly as the coding pack
maps `CoderEvent` → `SessionEvent`.

---

## Implementation status

| Layer | Status |
|---|---|
| Turn loop (`executor`) | Production |
| Coding pack tools + sandbox | Landed |
| Coding goal session (`coder-agent`) | Worker + optional planner + repair (signature routing) + critic + progress guards + gates + traces |
| Neutral Goal/Session types | **`liberado-session` crate** (GoalSpec, SessionEvent, hub) |
| Non-coding domain pack proof | **LifeOpsDemoRunner** (no coder-tools) |
| Unified session API for TUI/WebUI | **HTTP/SSE** `/api/goals*` on `liberado-server`; **one event vocabulary** with chat (converged 2026-07-11) |
| Meta-loop | Tuner + draft proposal export (Decision 14) |

Module layout in `coder-agent` (composition, not kernel): `roles`, `gates`, `critic`, `progress`,
`runtime`, `trace`, `lib` (backend orchestration only).

---

## Patterns stolen deliberately

Claude Code, Codex, OpenCode, Grok Build-class TUIs, Karpathy/bilevel loops, MAST/doom-loop research
— **harness patterns**, not framework imports. Liberado keeps its modular Rust kernel-and-packs
architecture.

---

## Design rules

1. **Kernel first, domain packs second** — coding is a pack, not the product identity.  
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
