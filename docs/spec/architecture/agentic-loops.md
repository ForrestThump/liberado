# Agentic Loops — Architecture

**Status**: living architecture, 2026-07-10  
**Implementation roadmap**: [`../../future-work/rust-native-agentic-coder-plan.md`](../../future-work/rust-native-agentic-coder-plan.md)  
**Hygiene audit**: [`../../future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md`](../../future-work/archive/agentic-mesh-hygiene-audit-2026-07-10.md)  
**Modularity**: [`modularity.md`](modularity.md) · **Contracts**: [`contracts.md`](contracts.md) ·
**Event-bus idea (annotated, partly superseded)**: [`meshify.md`](../../future-work/ideas/archive/meshify.md)  
**Channels & interactivity** (authority vs information graph; interactivity as a capability, not a subtype): [`channels-and-interactivity.md`](channels-and-interactivity.md)

Design inputs:
[`loop_architecture_reference_article.md`](../../future-work/ideas/archive/loop_architecture_reference_article.md),
[`doomloop_research.md`](../../future-work/ideas/archive/doomloop_research.md),
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
| **Loop** | time-based recurrence: wake on schedule → inspect state → one improvement pass → log → sleep | **never succeeds closed** — stops on cap, checker-satisfied-N-times, or human close | designed, not yet built — [`loops-plan.md`](../../future-work/loops-plan.md). A loop's body **is a goal**; the loop is a scheduler + series state, not a fourth engine |
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
[`doomloop_research.md`](../../future-work/ideas/archive/doomloop_research.md).

### Maker ≠ checker

Worker produces; **deterministic verifiers** then optional **critic role** on evidence (diff, logs,
artifacts). Never critic alone for success.

---

## Concurrency: what exists, what is reachable, and the ordering rule

**Audited 2026-07-24.** Prompted by the graph-engineering framing (fan out where work is
independent). The pieces for width exist; almost none of them are connected, and the order in
which they get connected matters more than the connecting does.

### Current state

| Capability | Machinery | Reachable by an agent? |
|---|---|---|
| Bounded-concurrent fan-out | `Orchestrator::dispatch_parallel` — semaphore over `tuning.dispatch.max_concurrent_subagents`, per-subagent capability narrowing, merged `Report` | **Yes, via the dispatch pack.** A `parallel_goals` payload constructs `SubDispatch`s. Optional `workspace_root` is forwarded to `RuntimeFactory::runtime_for_in`. |
| Expressing "these are independent" | — | **No.** `DispatchAction` is `{ExecuteDirect, DispatchSubagent, Clarify, Propose}`; `DispatchSubagent` is singular. A classifier cannot say *fan out*. |
| Background session + later callback | `hub.start_background` + `spawn_return_handoff` + `ChatSessions::append_note` | **No.** Wired to the human `/spawn` path (`POST /api/goals` with an `origin`), not to any tool. |
| Several tool calls in one turn | executor's tool loop | **Yes, but serialized** — `for call in &response.tool_calls { … runtime.invoke(call).await }` (`liberado-executor` `src/lib.rs`, both the streaming and non-streaming paths). No `JoinSet`, no `join_all`. |

Consequences worth stating plainly, because each is easy to assume otherwise:

- **An agent cannot run two subagents at once.** The model may emit two `delegate` calls in one
  turn, but the executor awaits each in sequence, so they are two consecutive waits.
- **`delegate` is synchronous by construction**, not by omission: it is `start_background`
  immediately followed by `await_terminal`. A chat turn needs the summary before it can reply, and
  delegated sessions have `AskHuman` stripped (D-e) precisely so a turn cannot become an unbounded
  wait on a human the face agent has no way to relay to mid-turn.
- **The async-with-callback pattern already works one layer up.** A human `/spawn` returns
  immediately and the summary folds into the parent conversation on terminal. Exposing that to
  agents is a non-blocking `delegate` variant, not new machinery.

### The ordering rule: isolation before parallelism

> **Do not make dispatch concurrent until workers are isolated.** Two agents writing one workspace
> race, and the failure is silent corruption of each other's work rather than an error.

This is not hypothetical and not fixable with prompting. Bun's Zig-to-Rust port fanned across many
agents sharing one git workspace; agents ran overlapping git commands and overwrote each other. The
fix was structural — forbid the unsafe commands, give each worker its own worktree.

Step 1 and step 2 of that sequence have landed. `coder-sandbox` ships `WorktreeWorkspace`
(PR #58). The coding pack already fans `payload.subtasks` onto worktrees (S6). The kernel/face
path is open through the dispatch pack (C7, PR #166): `dispatch_parallel` is reachable;
`delegate` is still synchronous. Do not expose a second fan-out API.

The original 2026-07-24 audit recorded no `WorktreeWorkspace`. Do not rebuild it.

So the sequence is fixed:

1. ~~**`WorktreeWorkspace`** in `coder-sandbox`~~ **Landed (PR #58).** Per-worker isolation
   and a defined merge-back. (`coding-tui-plan.md` G7/S6.)
2. ~~**A reachable fan-out**~~ **Landed (C7).** `dispatch_parallel` is reachable from the
   dispatch pack. The kernel forwards an opaque `workspace_root`; the pack supplies
   `WorktreeWorkspace`. `delegate` is untouched. Do not expose a second fan-out API.
3. **Decomposition in the classifier** — deciding *what* is independent, which is the genuinely hard
   part and the one worth doing last.

Before fanning out, the three questions that must have answers: where does each worker work, how do
results merge, and what happens when two disagree. A design that cannot answer all three is not
ready to be made concurrent.

**Also note:** making the executor's tool loop concurrent is not a `delegate` change — it affects
*every* tool. Tool concurrency and subagent concurrency are separate decisions with separate
blast radii, and conflating them would make a narrow feature into a workspace-wide one.

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
11. **Isolation before parallelism** — no concurrent workers until each has its own workspace and a
    defined merge-back. See §Concurrency; today only the *absence* of fan-out prevents the race.

Criteria intake and frozen verifier contracts: [`verifiers.md`](verifiers.md) §3.
