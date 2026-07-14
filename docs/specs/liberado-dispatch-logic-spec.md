# Liberado Dispatch Logic Spec — When the Dispatcher Routes, Executes, Reports, or Asks

**Status**: Completes the remaining detail of Tier-1 Decision 1 (Liberado invocation model).
Actionable; implementation can begin from here.
**Owner**: Shiloh Mangus
**Last Updated**: June 21, 2026
**Related**:
- `liberado-architecture-decisions.md` (Decision 1; Decisions 8, 11, 12, 16)
- `life-os-architecture.md` (§2 dispatcher/memory split)
- `liberado-permissions-idea.md` (capability/zone model — Decision 4)
- `liberado-vault-concurrency-spec.md` (write classes, provenance, correlation IDs)
- `liberado-memory-mcp` (renamed `liberado-tool-helper-mcp`: general + procedural memory)
- `turbomcp-client` (the dispatcher's MCP client: multi-transport, Arc-cloneable, sampling)

---

## 1. The Question This Answers

The dispatcher receives a **goal + minimal context** from the main agent and must choose exactly
one of four terminal actions:

1. **ExecuteDirect** — run the tool(s) itself and return a clean result.
2. **DispatchSubagent** — hand the goal to a narrowly-scoped subagent.
3. **Report** — terminal step of the above two: a filtered summary flows back to the main agent.
4. **Clarify** — ask the **main agent** (not the user) a structured follow-up question.

"How do we *ensure* it picks the right one?" You cannot make an LLM classifier perfect. So the
architecture instead guarantees three things that make the right behavior emergent and the wrong
behavior cheap:

- **Safe-by-default routing** — when confidence is low or consequences are high, the decision
  degrades toward Clarify or a proposal, never toward an irreversible action.
- **Deterministic guardrails that can only *downgrade* risk** — capability/zone/consequence checks
  run *after* the LLM and can turn ExecuteDirect → DispatchSubagent → Clarify, but never the reverse.
- **A learning loop + eval harness** — every decision is grounded in procedural memory and recorded
  back to it; the classifier is regression-tested against recorded fixtures (Decision 16).

This is the core design principle (see §3): **the cost of misrouting is asymmetric, so we bias
toward the cheap-to-be-wrong option.**

---

## 2. The Decision Pipeline

```text
goal + minimal context (from main agent)
        │
   (1) RETRIEVE   liberado-memory-mcp.search_tool_guidance(goal)   ← procedural memory
        │         + optional search_memory(goal) for user facts
        │         → may short-circuit to a high-confidence known strategy (skip step 2)
        ▼
   (2) CLASSIFY   small/fast inference → structured DispatchDecision { action, confidence, rationale }
        │         input: goal + minimal context + MCP catalog (names+short desc) + retrieved guidance
        ▼
   (3) GUARD      deterministic, downgrade-only checks (capabilities, zone write-class,
        │         consequence level, reaction-depth). Can lower the action's risk, never raise it.
        ▼
   (4) ACT        ExecuteDirect | DispatchSubagent | Clarify
        │         (Execute/Subagent both terminate in a filtered Report to main)
        ▼
   (5) RECORD     save_tool_guidance(outcome) + tracing span (Decision 12)
```

Disjoint context is preserved throughout (Decision 1, Decision 8): the dispatcher sees goal +
catalog + guidance; a subagent sees goal + its narrowed schemas + work context; the main agent sees
only the final Report. Minimal overlap = the real quadratic-prefill savings.

---

## 3. Why Safe-by-Default Works: the Asymmetric Cost Table

Misrouting is not symmetric. We design thresholds around the cost of each error:

| Actual best action | If misrouted to… | Cost | Severity |
|---|---|---|---|
| ExecuteDirect | DispatchSubagent | extra tokens/latency; correct result | **cheap** |
| ExecuteDirect | Clarify | one extra round-trip to main | **cheap** |
| DispatchSubagent | ExecuteDirect | dispatcher does shallow work, may miss steps / pollute its own context | medium |
| Clarify (ambiguous) | ExecuteDirect | **acts on a wrong interpretation** — possibly an irreversible write | **expensive** |
| proposal_only goal | ExecuteDirect | **autonomous high-consequence action** (family/calendar/external) | **expensive** |

The expensive errors all share one shape: *taking an action that should not have been taken
autonomously*. So the bias is explicit: **when uncertain, prefer the action further down this list
(Subagent > Direct, Clarify > Direct).** The deterministic guards in §6 enforce the two "expensive"
rows as hard rules so they never depend on classifier judgment at all.

---

## 4. The Structured Decision Output

The classifier emits a typed value (JSON via the provider's structured-output mode), never free
prose. This makes the decision inspectable, loggable, testable, and acted on deterministically.

```rust
pub struct DispatchDecision {
    pub action: DispatchAction,
    /// 0.0–1.0 self-reported confidence in the classification.
    pub confidence: f32,
    /// One-line why, for tracing + procedural-memory recording (never shown to the user).
    pub rationale: String,
}

pub enum DispatchAction {
    /// Run these tool calls myself, then Report. Only for simple/low-consequence goals.
    ExecuteDirect {
        calls: Vec<ToolCall>,                 // tool name + args; ≤ SMALL_FANOUT calls
    },
    /// Hand off to a subagent with a narrowed grant + disjoint context.
    DispatchSubagent {
        goal: String,                         // restated, self-contained
        capabilities: CapabilitySet,          // base ∩ narrowing — never widened (Decision 4)
        allowed_mcps: Vec<String>,            // filtered catalog the subagent may see
        success_criteria: Vec<String>,        // how the subagent knows it's done
        artifact_target: Option<String>,      // e.g. "reviews/", "decisions/" (agent_writable zone)
        model: Option<ModelChoice>,           // may differ from dispatcher/main
        correlation_id: String,               // ties writes to this goal (loop-breaking + idempotency)
    },
    /// Ask the MAIN AGENT (not the user) to resolve before any action is taken.
    Clarify {
        questions: Vec<String>,
        what_blocked: BlockReason,            // Ambiguous | MissingParam | CapabilityGap | LowConfidence | DepthLimit
    },
}
```

`Report` is not an action variant — it is the **return type** of executing Execute/Subagent (§7).

---

## 5. Classification Criteria (what the model is told to weigh)

The classifier's system prompt encodes these signals. They are heuristics for the LLM, made
*safe* by the deterministic guards in §6 — the model proposes, the guards dispose.

**Choose ExecuteDirect when ALL hold:**
- Single zone, read or low-consequence write (zone write-class `shared`/`agent_writable`, §6).
- ≤ `SMALL_FANOUT` tool calls (default 3), no branching judgment between them.
- Deterministic success (you can tell from the tool result whether it worked).
- Procedural memory has a matching, previously-successful strategy, **or** the goal is trivially
  mapped to one obvious tool.

**Choose DispatchSubagent when ANY hold:**
- Multi-step, or spans multiple zones/MCPs, or needs intermediate reasoning/verification.
- Open-ended ("review…", "figure out…", "propose…") or produces a vault artifact.
- Would otherwise pull a lot of tool output into the dispatcher's own context.
- Procedural memory says this task_type historically needed a subagent.

**Choose Clarify when ANY hold:**
- The goal is **ambiguous** (more than one reasonable interpretation that lead to different writes).
- A **required parameter is missing** and is not in the dispatcher's minimal context and cannot be
  safely defaulted.
- Confidence `< CLARIFY_THRESHOLD` (default 0.6).
- (The CapabilityGap / DepthLimit cases are produced by the guards in §6, not the model.)

**Procedural-memory short-circuit (token saver):** if `search_tool_guidance` returns a high-score
exact directive for this `task_type` ("Use tasks-mcp:add for shopping-list adds"), the dispatcher may
emit `ExecuteDirect` **without a classification inference call** — guidance + guards are enough.
This is where the system gets cheaper *and* more reliable as procedural memory grows.

---

## 6. Deterministic Guardrails (downgrade-only)

Run after classification, in code, no inference. Each can only move the action toward *less*
autonomy. This is what makes the two "expensive" rows of §3 impossible regardless of classifier error.

1. **Capability check.** For every `ToolCall` / requested MCP, verify the grant exists in the
   active `CapabilitySet`. Missing → if narrowing could be requested, downgrade to `Clarify{CapabilityGap}`;
   never auto-widen (Decision 4 invariant).
2. **Zone write-class check** (`liberado-vault-concurrency-spec.md` §3):
   - target zone `human_only` → reject the write; `Clarify` or drop.
   - target zone `proposal_only` → **force** the result to a proposal artifact for human approval
     (Decision 11); an `ExecuteDirect` mutation here is rewritten to "write proposal," and a
     `DispatchSubagent` must carry `artifact_target` in a review location, not the live zone.
   - `shared`/`agent_writable` → allowed (still via optimistic `expected_hash`).
3. **Consequence gate.** Actions tagged high-consequence (external comms, anything touching
   `Sensitive`/`FamilyShared`, irreversible deletes) may never be `ExecuteDirect`; minimum is a
   proposal or a subagent that emits a proposal.
4. **Reaction-depth guard** (background path). If this dispatch is itself a reaction and the
   correlation chain depth ≥ `MAX_REACTION_DEPTH` (spec §6.4), force `Clarify{DepthLimit}` /
   proposal instead of acting — stops runaway cascades.
5. **Confidence floor.** `confidence < CLARIFY_THRESHOLD` and action ≠ Clarify → downgrade to
   `Clarify{LowConfidence}`.

Order: a decision passes all five; the *most restrictive* downgrade wins.

---

## 7. The Reporting Contract (what flows back to main)

Both Execute and Subagent terminate in a **Report** to the main agent. The whole point of the
dispatcher is that the main agent's context never sees tool schemas, raw tool output, or internal
dispatch reasoning. The Report carries only:

```rust
pub struct Report {
    pub outcome: Outcome,                 // Succeeded | PartiallySucceeded | Failed | Proposed
    pub summary: String,                  // high-signal, human-readable, short
    pub artifacts: Vec<String>,           // vault paths written (e.g. "reviews/2026-06-21.md")
    pub new_high_signal_facts: Vec<String>, // things worth surfacing into ContextPolicy
    pub follow_up: Option<String>,        // optional suggested next step for main
}
```

- **ExecuteDirect → Report**: the result, summarized. Never the raw tool JSON.
- **DispatchSubagent → Report**: a pointer to the artifact + a short summary. The subagent's full
  trace stays out of main context (Decision 8); it lives in tracing/audit, not the conversation.
- **Proposed outcome**: tells main "I prepared X for your approval at path Y" rather than "done."

The main agent decides how much of the Report to surface to the user.

---

## 8. Why Clarify Goes to the Main Agent, Not the User

The dispatcher deliberately runs with **less** context than the main agent (disjoint partitions).
So when it's blocked on ambiguity or a missing parameter, the **main agent very often already holds
the answer** (it has the full conversation, the user's current focus, ContextPolicy state). Clarify
returns to main; main resolves it from its own context and re-dispatches **without ever bothering
the user**. Only if the main agent *also* can't resolve it does the user get asked.

This is what makes Clarify cheap enough to be the safe default: it's usually a single in-system
round-trip, not a human interruption. It also keeps the human-in-the-loop burden low (a core goal).

---

## 9. Closing the Loop: Learning + Measurement

- **Record outcomes** (step 5): after acting, `save_tool_guidance` with the `task_type`, the action
  taken, `tools_used`, and success/failure. Future goals of the same shape hit the §5 short-circuit.
  Failed strategies are recorded too (as cautions), so the dispatcher stops repeating them.
- **Tracing** (Decision 12): every decision emits a structured span — `goal hash, retrieved
  guidance ids, action, confidence, rationale, guard downgrades, outcome`. This is the data that
  tells us whether routing and the quadratic savings are actually working.
- **Eval harness** (Decision 16): record `(goal, minimal_context) → DispatchDecision` fixtures with a
  mocked provider. The classifier is nondeterministic, so we pin behavior with a fixture suite and
  catch regressions when prompts/models change. **This is the concrete answer to "how do I ensure
  it does the right thing": you make the decision a testable, versioned artifact, not a vibe.**
- **Automated tuning** (`liberado-heuristics-tuner`, added 2026-07-03): automates the "run the eval
  suite → read the misses → tweak the system prompt → run again" loop this section otherwise
  describes doing by hand. Scores candidate prompts against `liberado-eval`'s scenario set (or, for
  the executor/subagent layers, against a real mocked tool-loop run) via a beam-search-with-restarts
  loop, and proposes the best candidate as a diff + rubric for human review — never auto-merges
  (same Decision-14 trust boundary as everything else that touches config/prompts). Full design:
  `docs/roadmap/heuristics-tuning-engine-plan.md`. Also surfaced a project-level open reliability
  finding beyond prompt tuning itself: `docs/roadmap/archive/multi-step-execution-reliability-finding.md`.

---

## 10. Execution Concurrency & Inference Location

Two orthogonal axes are often conflated here; keeping them separate dissolves the apparent
tension between "in-process" and "the main agent should make progress while a subagent runs."

**Axis 1 — inference location (in-process).** The dispatcher's classification inference runs
**in-process inside the daemon**, calling the provider abstraction directly. It is core
orchestration — it needs tight access to the provider, the MCP client pool, the active
`CapabilitySet`, and tracing — so routing it back through the MCP protocol would be pure ceremony.
**MCP sampling (`turbomcp-client`) is reserved for genuinely external servers** (MCPs/hooks that need
to borrow the daemon's inference without holding provider keys). The dispatcher is not external.

**Axis 2 — concurrency (async, not blocking).** In-process does **not** mean synchronous. The daemon
is tokio-async; the dispatcher runs as spawned tasks. The daemon itself never blocks — hooks keep
firing and other input is still handled while any dispatch is in flight. Work splits by timescale:

- **Classification** (retrieve guidance → classify → guards) is fast (small model, sub-second) and
  effectively synchronous from the caller's view. Never detached.
- **Execution** varies; only `DispatchSubagent` is meaningfully long. Two modes, expressed as a field
  on the dispatch (not a separate code path):
  - **Await** (default, foreground): the *conversational turn* awaits the Report because the user is
    waiting for it. The daemon stays free; only this one turn is pending.
  - **Detach** (background-originated work, or foreground work the main agent flags non-blocking):
    the dispatcher returns a job handle immediately (`correlation_id` + status), the main agent
    completes its turn, and the subagent's Report is delivered later as a high-signal event via the
    **same vault-mediated surfacing path hook messages use** (Decision 9). Daemon-first (Decision 2)
    is exactly what makes this clean.

**Promote-on-timeout.** A foreground dispatch starts in Await; if still running after
`DETACH_SOFT_TIMEOUT`, the daemon auto-promotes it to Detach — the main agent says "this is taking a
while, I'll surface it when done" and frees the turn. Snappy for fast work, graceful for slow work,
with no upfront duration guess.

**Concurrency bound.** Subagents share KV-cache pressure on local inference (Decision 8), so
in-flight subagents are capped at `MAX_CONCURRENT_SUBAGENTS`; excess dispatches queue. A detached job
that completes while the cap is full still queues its *successor* work, not its Report delivery.

```rust
pub enum ExecMode { Await, Detach }   // chosen per dispatch; Await may be promoted to Detach

pub struct JobHandle {                // returned immediately for Detach (and on promotion)
    pub correlation_id: String,
    pub status: JobStatus,            // Queued | Running | Done | Failed
}
```

---

## 11. Tunables (single source of truth — Decision 14)

| Name | Default | Meaning |
|---|---|---|
| `SMALL_FANOUT` | 3 | Max tool calls allowed in `ExecuteDirect`; more ⇒ multi-step ⇒ subagent. |
| `CLARIFY_THRESHOLD` (read / no mutation) | 0.5 | Below this confidence on a read-only action, downgrade to `Clarify`. Cheap to be wrong. |
| `CLARIFY_THRESHOLD` (agent_writable write) | 0.7 | Higher bar before writing. (proposal_only / high-consequence are forced to proposals by the §6 guards regardless of confidence.) |
| `MAX_REACTION_DEPTH` | 4 | Correlation-chain depth before a cascade is halted (shared with concurrency spec). |
| `MAX_CONCURRENT_SUBAGENTS` | 2 | In-flight subagent cap (KV-cache / homelab bound); excess queues. |
| `DETACH_SOFT_TIMEOUT` | 20s | Await → Detach promotion point for foreground dispatches. |
| `dispatcher_model` | fast/cheap | Model for classification (provider-agnostic; may differ from main & subagents). |
| `guidance_match_floor` | tuned | Procedural-memory score above which the §5 short-circuit fires. |

---

## 12. v1 Scope vs Later

**v1 (ship the routing skeleton):**
- The pipeline (§2), `DispatchDecision`/`Report` types, the five guards (§6).
- In-process dispatcher inference; in-process subagents with capability filtering (Decision 8).
- Await execution + Detach for background work + promote-on-timeout (§10).
- Procedural-memory retrieve + record loop.
- Tracing spans + a starter eval-fixture suite.

**Deferred:**
- Procedural-memory short-circuit tuning (start always-classify; add short-circuit once memory has
  signal).
- Out-of-process subagents.
- Multi-step subagent planners beyond a single delegated goal.

---

## 13. Open Questions (non-blocking)

1. **Resolved** — dispatcher classification inference runs **in-process** in the daemon via the
   provider abstraction directly (§10, Axis 1). MCP sampling through `turbomcp-client` is reserved
   for external MCPs/hooks that need reasoning without provider keys. Concurrency is provided by the
   async runtime + Await/Detach (§10, Axis 2), not by relocating inference.
2. Should `confidence` be model-self-reported (cheap, noisy) or derived from guidance-match score +
   heuristics (more stable)? Start self-reported; revisit with eval data.
3. Where do failed-strategy "cautions" live — procedural memory with a `success: false` tag, or a
   separate store? Leaning a tag on the existing procedural store.
