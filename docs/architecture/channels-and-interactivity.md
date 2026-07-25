# Channels & interactivity — three channels, two decisions

**Status**: living architecture, 2026-07-12.
**Frame source**: the agent-pools research (four model passes) +
[`agent_pools_research_results.md`](../../agent_pools_research_results.md) `# Followup` — the
**authority-graph vs information-graph** distinction.
**Related**: [`agentic-loops.md`](agentic-loops.md) · [`contracts.md`](contracts.md)
(`EventSource`, `CapabilitySet`, `DomainPackRunner`) ·
[`session-focus-plan.md`](../roadmap/archive/session-focus-plan.md) (D7 unified `Session`, S1–S5) ·
[`meshify.md`](../ideas/archive/meshify.md) (partially-superseded event-bus idea) ·
[`architecture-alignment-audit-2026-07-11.md`](../roadmap/archive/architecture-alignment-audit-2026-07-11.md).

---

## The frame: a system has three channels, and they must stay separate

Nearly every multi-agent framework fails the same way: letting agent A *talk to* B implicitly lets A
*direct* B, so authority leaks through the communication path. The fix is to notice that a system
actually has **three distinct channels**, and to never let one impersonate another:

| Channel | Carries | Owner | In Liberado today |
|---|---|---|---|
| **Authority** | *who may do work, with what capabilities* | the **Dispatcher** (+ `CapabilitySet`, narrowing only) | built — only the dispatcher creates work and grants authority |
| **Information** | *what changed in the world* (facts) | trusted infra (the `EventSource` → daemon event pipeline) | built for **external** facts (cron / vault-watch / webhooks); **internal** facts deferred |
| **Human-input** | *a pack asking a human at a crossroads* | the session kernel (`InputChannel`) | built (session-focus S1: `AwaitingInput` / `HumanInput` / idle budget) |

**The one invariant across all three: none of them lets one agent gain authority over another.**
Facts are notifications, not commands. A human answer is guidance, not a capability grant. The
dispatcher is the *only* thing that turns any signal into new, capability-scoped work.

Liberado already separates these — this doc makes the separation explicit and records two decisions
that were previously implicit.

---

## Decision A — Interactivity is a *capability*, not a session subtype

**The dilemma it kills.** "Should a coding session be interactive?" is unanswerable by the system: a
strong model with a goal can in principle finish autonomously, and only the *human* knows whether
they want to babysit this particular task. So the session must not decide. Making "interactive" a
session **type** would force exactly that impossible decision.

**The resolution.** Interactivity is not one knob — it's three independent attributes on a
`Session` (fits D7: attributes, not subtypes):

| Attribute | Question | Mechanism |
|---|---|---|
| `AskHuman` | *May* the pack reach out at a crossroads? | a real **`Capability`**, granted per session profile (**done — S6**) |
| `visibility` | Is anyone *there* to answer? | derived from `origin`: `/spawn` or a human-attended chat turn ⇒ foreground; cron/hook ⇒ background |
| budget (`max_turns` / `max_idle_secs`) | How long before it gives up? | already on `GoalSpec` |

The **channel** those attributes gate is the S1 primitive — already shipped and durable. There is
nothing new to *build* for "let the model ask a human when it hits a crossroads"; there is a flag
that says *whether it may*, and the surrounding context decides *whether anyone answers*.

**As of S6 this is enforced, not merely declared.** `Capability::AskHuman` is an ordinary capability
in the usual grant machinery, so a session's `SessionGrant` either carries it or does not, and the
kernel acts on that: **without it, the pack is handed an already-closed `InputChannel`** — it cannot
block on a human even if it wants to — and `POST /api/goals/{id}/message` answers **403 Forbidden**
("never allowed"), which is deliberately distinct from **409** ("too late, it finished"). Notice what
this buys: `payload.interactive` is only a *request*. A caller can ask for an interactive session all
it likes; if the grant withholds `AskHuman`, the session runs to completion without ever prompting.
Interactivity is therefore not something a caller can assert — it is something the authority model
*permits*. That is the whole content of "interactivity is a capability, not a subtype."

**Consequences (these settle open questions):**

- **`/spawn <domain> <goal>` is the interactive trigger.** A human explicitly launches a foreground
  session with `ask_human: on`. The **dispatcher never classifies interactivity** — it can't know
  what the human wants, so it doesn't try.
- **`SessionOffered` (S4) is the one exception, and it's still human-gated**: a chat turn (a human
  *is* present) whose `delegate` decides the work is better done hands-on emits an offer; the human
  accepts with `/join`. Not an auto-offer from background dispatch.
- **A background (cron/dispatcher-spawned) subagent** runs with `ask_human: off`, or *on* with its
  questions **queued** against `max_idle_secs`. No human is watching, so it hits the goal or
  exhausts budget; an abandoned question is reaped by the idle budget (already implemented).
- **"5-turn subagent" and "long-running goal session" are the same thing**, differing only by
  `max_turns`. Crons always carry a goal. No new types.

**Build status:** channel + idle reaper = **done (S1)**; `SessionOffered` wire vocabulary = **done
(S4 foundation)**; `/spawn` trigger + return handoff = **done (S4)**; `Capability::AskHuman` on
session profiles = **done (S6)** — kernel-enforced, with the 403/409 split above.

---

## Decision B — The information graph *is* the `EventSource` fact bus; internal facts are deferred; spawning stays centralized

**External facts are already built and already non-blocking.** Cron, vault-watch, and webhooks are
`EventSource`s that **publish a fact and return** onto `Daemon::event_sender()`; the reactive
pipeline (dispatch → orchestrate) decides whether to spawn. **A cron does not own an LLM and does not
lock the daemon** — the "loops shouldn't block the main agent" concern is designed out already.

The followup's recommendations are therefore mostly "generalize what exists," and we adopt them as
standing rules:

- **Facts, not agents.** Publish `filesystem.changed`, never "B, go reindex." A subscriber decides,
  under its *own* narrowed authority, whether to act. (This is the existing typed `Event` vocabulary.)
- **An event wakes the *dispatcher*, not an agent.** Routing wake-ups through the dispatcher keeps it
  the sole authority that can create work and grant capabilities — this is what prevents an emergent
  swarm (`A → wakes B → B spawns C → …`).
- **Typed schema, not arbitrary topics.** Events are an `enum`/typed payload, not a second LLM chat
  channel.

**Internal facts are named but deferred.** The genuinely *new* subsystem would be **internal** facts
— one subagent's output reactively waking other work through the dispatcher. We do **not** build this
speculatively (all four research passes converged on: don't build reactive/peer coordination until a
concrete workload the sequential `delegate` model cannot express). **Gate:** build the internal fact
bus only when there is a real case where "A finishes ⇒ this should reactively trigger B" that
`delegate` (dispatcher runs A, then B) cannot already express.

**No peer channels — ever.** "Two agents with different-but-related responsibilities" coordinate
through **facts + centralized spawn** (A publishes; the dispatcher spawns B on match; they never
talk), or through **delegation** (needs another agent's work) or the **human-input channel** (needs a
human's answer). There is no fourth case requiring A and B to hold a conversation. This is consistent
with [`meshify.md`](../ideas/archive/meshify.md)'s rejected step 5 (no peer mesh; the runtime is a hub around
one daemon) and the agent-pools verdict.

---

## What this changes on the roadmap

- **S4 trigger re-scoped to `/spawn`** (human-initiated), *not* dispatcher interactivity
  classification. `SessionOffered` is retained for the human-attended offer case only. This is a real
  simplification — no dispatch-side "is this interactive?" heuristic to build.
- ~~**New small item:** an `ask_human` capability flag on sessions / hat profiles (Decision A)~~ —
  **shipped in S6** as `Capability::AskHuman`, granted per `[[session_profiles]]` entry and enforced
  in the kernel (closed input channel + 403). It turned out to be the *load-bearing* piece of S6, not
  a footnote: goal sessions had no authority boundary at all before it, so there was nothing for a
  "narrower grant" to narrow.
- **Deferred + gated:** the internal fact bus (Decision B). Not on the near roadmap; documented so
  future work has a home and a trigger condition, not a standing invitation to over-build.

## Invariants (non-negotiable)

1. **Only the dispatcher grants authority** (narrowing only; `CapabilitySet`).
2. **Information ≠ authority** — a fact never carries a capability; an answer never widens one.
3. **No standing peer authority; no agent-to-agent command channels.**
4. **Spawning is centralized** — every path to new work runs through the dispatcher's capability check.
