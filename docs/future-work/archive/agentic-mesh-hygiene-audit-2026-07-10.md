# Agentic mesh hygiene audit — 2026-07-10

**Scope**: `coder-*` crates, related executor/orchestrator seams, and architecture/roadmap docs for
the agentic orchestration work.  
**Goals**: find duplication, tight coupling, decomposition opportunities, outdated docs; check that
the plan does not pigeonhole Liberado into a coding-only product.

Related: [`agentic-loops.md`](../../architecture/agentic-loops.md),
[`modularity.md`](../../architecture/modularity.md),
[`rust-native-agentic-coder-plan.md`](../rust-native-agentic-coder-plan.md),
[`meshify.md`](../../ideas/archive/meshify.md).

---

## Verdict (short)

| Area | Status |
|---|---|
| Inner loop substrate (`executor`, `ToolRuntime`, `Provider`) | Mesh-friendly, reusable |
| Coding domain pack (`coder-tools`, `coder-sandbox`, PR factory) | Correctly specialized |
| Goal-session / multi-role / attempt / critic / progress | **Implemented inside coding crate names** — works, but vocabulary risks pigeonholing |
| Docs | Mixed: plan says general; crate map / overview still read as “coding product” |
| Coupling | Mostly healthy DAG; `coder-agent` is a monolith (~1.4k LOC single file before split) |
| Premature abstraction | Avoid extracting empty `agent-session` crates *now*; **do** design neutral seams and keep coding as a domain pack |

**Product intent (confirmed)**: Liberado is a **general agentic orchestration mesh**. Coding is the
**first domain pack** and the reliability gate for self-improvement (PR factory). It is not the
center of gravity.

---

## Dependency DAG (actual)

```
liberado-common, liberado-provider, liberado-executor
        ▲
        │
coder-core  (coding contracts only → common)
        ▲
   ┌────┴────┐
coder-sandbox   (workspace/command isolation)
        ▲
coder-tools     (ToolRuntime limb)
        ▲
coder-agent     (coding goal-session composition)
        ▲
coder-runner    (process adapter)
        ▲
pr-dispatch MCP (consumer)
```

**Healthy rules already held:**

- `coder-core` does not depend on tools, sandbox, agent, or PR factory.
- Tools do not own the loop; agent does not own forge/PR lifecycle.
- Surfaces (PR factory process boundary) consume `CoderRunRequest`/`Result`, not internals.

**Risks:**

1. **Vocabulary gravity** — `CoderEvent`, `CoderTask`, `ProgressPolicy` live in `coder-core`. A
   non-coding goal session will either duplicate them or force an awkward dependency on “coder”
   crates. That is the main pigeonhole risk.
2. **Composition gravity** — attempt loop, critic, role loading, and progress guards live in
   `coder-agent`. A life-ops goal would re-copy that graph unless a domain-neutral session kernel
   exists (later crate or `common` types).
3. **Dual event systems** — chat has `AgentEvent` (`executor` / `main-agent`); coding has
   `CoderEvent`. TUI/WebUI will not want two session languages forever.
4. **Progress vs doom-loop overlap** — executor already does near-duplicate tool / short-cycle
   guards; `coder-agent` progress guards add no-mutation / validation-churn. Complementary axes
   (good), but docs must not treat them as competing loops.

---

## Duplication findings

| Item | Where | Assessment |
|---|---|---|
| Git status / diff | `coder-tools` (model-facing) + `coder-agent` (backend gates) | **Intentional**: model path is policy-gated; backend gates must not be. Keep split; share helpers only if pure parse functions (e.g. porcelain path parse). |
| Doom-loop vs progress guards | `executor` vs `coder-agent/progress` | **Complementary**, not duplicate. Executor = arg-similarity/cycles; progress = domain progress metric. |
| Role prompt loading | `coder-agent` only | Fine for now; when planner lands, keep one `role_instructions()` helper (done). |
| Report / Outcome mapping | `CoderRunResult::report()` | Correct mesh boundary. |
| Mock git repo test harness | `coder-agent` tests | Local to crate; OK until second crate needs it → `test-support`. |
| Event preview / session_id / trace write | `coder-agent` | Generic session utilities living under coding — extraction candidate later. |

---

## Coupling findings

| Coupling | Severity | Note |
|---|---|---|
| `coder-agent` → tools + sandbox + executor + provider | Expected | Domain orchestrator may compose limbs |
| `coder-tools` → `liberado-executor` / `provider` for `ToolRuntime`/`ToolDef` | Acceptable | ToolRuntime is the mesh limb trait |
| PR factory → process JSON of `coder-core` | Good | Loose process seam |
| Docs calling the product “agentic coder” | **Doc coupling** | Pushes readers toward coding-as-center |
| “Extract Goal types only when second domain needs them” | **Design debt** | Deferred extraction is fine; deferred *design* of seams is not |

---

## Decomposition opportunities

### Do now (low risk)

1. **Split `coder-agent` modules** — `progress`, `gates`, `critic`, `trace`, `roles` (composition stays in `lib.rs`). Reduces god-file without new crates.
2. **Doc frame: kernel vs domain pack** — architecture + plan + modularity + overview.
3. **Mark coding types as domain specialization** of logical `Goal` / `SessionEvent` / `TerminalState` in architecture (even while types still live in `coder-core`).

### Do soon (when it pays)

4. **Neutral session vocabulary** in `liberado-common` or a tiny `liberado-session` crate:
   - `Goal`, `TerminalState`, `SessionEvent` (or shared envelope), `AttemptRecord`
   - Coding maps `CoderTask` → `Goal`, `CoderEvent` → `SessionEvent`, results → `Report`
5. **Converge chat + coding event streams** for TUI/WebUI (one SSE vocabulary with domain payloads).
6. **`Verifier` trait** (code-owned gates) as a mesh concept; coding implements diff/validate;
   life-ops implements artifact/MCP checks.
7. **Generic progress guard** parameterized by “progress metric moved?” callback — coding supplies
   mutation/diff; other domains supply their metric.

### Do not do yet

- Big-bang rename of all `coder-*` crates.
- Event bus rewrite solely for coding.
- Merging PR factory into workspace before loop reliability is proven.
- Absorbing MCP tools into `coder-tools`.

---

## Outdated / misleading docs (to fix in this pass)

| Doc | Issue |
|---|---|
| `overview.md` coder-agent row | Still says “MVP: coder worker only; planner/critic/repair next” — critic/repair/guards exist |
| `agentic-loops.md` status table | Same understatement; weak mesh dependency rules |
| `modularity.md` | “Extract only when second domain” undersells designing neutral seams now |
| Plan filename `rust-native-agentic-coder-plan.md` | Name is coding-shaped; keep file path for links, title/intro must say orchestration mesh |
| `coder-agent` Cargo.toml description | Still “MVP” |
| Dual “center of gravity” language | Must repeatedly state: mesh first, coding is a domain pack |

---

## Alignment checklist: general framework, not coding silo

| Principle | Required behavior |
|---|---|
| Kernel owns control flow | Goal session, budgets, terminals, subagent fan-out, attempt log |
| Domain pack owns limbs | `ToolRuntime`, sandbox/env, verifiers, progress metric, role prompts |
| Surfaces own UX only | TUI/WebUI/CLI/PR MCP subscribe to events; no loop ownership |
| Mesh types at boundaries | `Report`, `Outcome`, `CapabilitySet`, `Provider` — already shared |
| Second domain proof | A non-coding goal with MCP tools + automated criteria must reuse kernel without depending on git/diff |
| Config | Domain packs load their own role/policy sections; kernel loads budgets/terminals |

**Second-domain smoke (future, not blocking coding):** e.g. “file this vault note and mark task done”
with TurbomcpRuntime + vault verifiers + turn budget — same session machinery, no `coder-tools`.

---

## Action items from this audit

| # | Action | Owner slice |
|---|---|---|
| A | Refresh architecture/plan/modularity/overview for kernel vs domain pack | Docs (this pass) |
| B | Module-split `coder-agent` | Code (this pass) |
| C | Fix outdated status lines (critic/repair/guards landed) | Docs (this pass) |
| D | When planner lands, keep it domain-agnostic API if possible | Later code |
| E | Extract neutral session types when a second domain would otherwise copy `coder-core` | Later code |
| F | Unify event vocabulary for TUI session API | Phase 8-ish |
| G | Delete VTCode backend when Liberado loop is sole path | PR factory cutover (default is Liberado; legacy override remains) |
| H | Coder eval layer in `heuristics-tuner` after live smokes | Meta-loop automation |

---

## What we deliberately leave alone

- Executor doom-loop implementation (proven; complementary to progress guards).
- Process boundary `liberado-coder-run` (good adapter pattern for nested workspaces).
- Host vs Docker sandbox split.

---

## Follow-up — 2026-07-11 architecture-alignment audit

Status of the action items above, verified against the working tree:

| # | Action | Status |
|---|---|---|
| A | Doc reframe kernel vs domain pack | ✅ done (overview/plan/modularity/agentic-loops all carry it) |
| B | Module-split `coder-agent` | ✅ done (`roles`, `gates`, `critic`, `progress`, `trace`, `runtime`, `verify_pipeline`, …) |
| C | Fix outdated status lines | ✅ done |
| E | Extract neutral session types | ✅ done early — `liberado-session` (GoalSpec/SessionEvent/hub) exists; `coder-agent` and `server` consume it |
| F | Unify event vocabulary | ✅ done 2026-07-11 — one `SessionEventKind` wire vocabulary for chat + goals (`chat-client-contract`), one decoder for all surfaces; `AgentEvent` stays the inner-loop tap, mapped at the server boundary |
| D/G/H | planner API / VTCode deletion / coder eval layer | in flight per plan |

**New finding (supersedes "avoid premature extraction" for verify types only):**
`config-loader` gained a *real* dependency on `coder-core` (`VerifierSpec`/`PipelinePolicy` in
`config-loader/src/model.rs`) — the pigeonhole risk this audit warned about (risk 1) materialized
one layer lower than predicted, in config rather than a second domain pack. Extraction of the
neutral verify vocabulary into `common`/`liberado-verify` is now warranted. Full analysis:
[`architecture-alignment-audit-2026-07-11.md`](architecture-alignment-audit-2026-07-11.md).
- Draft-PR-only self-extension gate.
