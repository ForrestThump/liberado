# a2a-protocol-idea.md — Agent2Agent (A2A) Interop

**Status**: Idea, not decided. Nothing here is scheduled; captured so the seams that already exist
for it aren't lost, and so A2A isn't confused with the unrelated Agent Client Protocol from Zed.

**Research update (2026-07-04, `agent_pools_research_results.md`, four independent research
passes)**: this doc's own instinct — A2A as *external* interop, not an internal coordination
model — came back confirmed, not just assumed. **Internal A2A-style authority-splitting (multiple
independently-authoritative peer agents negotiating/coordinating) is a confirmed bad fit**, not
merely deferred: coordinating between independently-authoritative peers requires *some* arbiter
with superset authority to resolve conflicts, which directly breaks the narrow-only capability
invariant (Decision 4) this whole system is built on. Every source researched (including
Anthropic's own published multi-agent research system) converges on staying with
orchestrator + narrowed-workers (what Liberado already has, and what the "pool" work — Decision 18
checkpoint #3's second half — extends) rather than peer coordination, until a *concrete* workload
demonstrates the simpler model fails. None has appeared. A **life-os-to-other-systems** A2A bridge
— Liberado as *one bounded system* talking to *other* bounded systems over the open protocol — is
unaffected by this and remains the legitimate version of this idea; everything below in this doc
was already written with that framing, not the internal one, so it stands as originally captured.

One adjacent idea surfaced by the research, worth naming so it isn't lost either but is **not**
part of this doc's scope: mediated pub/sub *data* notification between subagents (a subagent
subscribes to a topic and gets woken when relevant data changes) — explicitly not an
authority-sharing mechanism, since subscriptions and wake-ups would still gate through the
dispatcher's normal capability check, not agent-to-agent trust. Judged low-risk and worth
considering *if* a concrete need shows up once pools exist — not before, and not the same thing as
A2A.

**Goal**: Let Liberado talk to *other* agent systems (not just its own MCPs/subagents) using the
open **Agent2Agent (A2A)** protocol — a JSON-RPC/HTTP spec (Linux Foundation, ex-Google) for
peer-to-peer agent interop: an **AgentCard** (capability discovery document), a **Task** lifecycle
(submit / poll / stream / cancel), and typed message/artifact exchange between agents built on
different stacks.

---

## Naming note — resolve before writing any code

Liberado's background-trigger system is now called **hooks** (thin HTTP webhook receivers; see
`life-os-architecture.md` §4 — formerly "ACP," renamed to avoid initialism collisions). The
industry has a **different** "ACP" — the **Agent Client Protocol** (Zed/editor integration, noted
as a lower-leverage gap in `vs-hermes.md`) — which is unrelated to both Liberado's hooks and to
A2A. When this is picked up: name the crate/module something unambiguous, e.g. `liberado-a2a` or
`a2a-bridge`, and spell out "Agent2Agent (A2A)" on first use in any doc that also mentions hooks or
the Zed Agent Client Protocol — the two should not be conflated.

## Why the existing infrastructure already carries most of this

This was the open question raised 2026-07-01: does JSONL conversation storage and the current
mesh direction support A2A and more agent-triggered background work later? Short answer: **yes,
by design** — the seams were built for exactly this shape of extension, per Decision 17
(`liberado-conversation-store-spec.md`) and Decision 18 (`meshify.md`):

- **`author` identity on every message node** (not just `user`/`assistant`/`tool`) is the seam
  Decision 17 names explicitly for "multi-agent / debate." A remote A2A peer is just another
  `Author` variant — no schema change.
- **`ConversationHeader.parent_conversation` / `spawned_by` lineage** already expresses "this
  conversation was spawned by another agent/dispatch" — the same shape an inbound A2A `Task`
  needs (a task delegated *to* Liberado, or *by* Liberado to a peer, is a conversation with
  lineage back to whoever asked).
- **The `ConversationStore` trait** means none of this requires touching the storage engine —
  JSONL stays fine at homelab/single-tenant scale (§5 of the spec: Postgres is the swap-in *only*
  if Liberado ever goes multi-process/multi-tenant, which A2A alone does not force).
- **The capability/zone model (Decision 4) + consequence gating (Decision 1 guards)** is the
  natural place to gate what an *external* agent can ask Liberado to do, and what Liberado is
  allowed to hand to an *external* agent — same enforcement points MCPs already go through, not a
  new security model.
- **The mesh direction (Decision 18)** — components publish/subscribe events rather than calling
  each other directly — means an A2A bridge is "just another event-source in, just another
  capability out," not a special case bolted to the daemon.

## What's actually new (the real gap)

Two new protocol *surfaces*, distinct from anything in `docs/spec/reference/api.md` today:

1. **Inbound**: an A2A server exposing an `AgentCard` + `Task` endpoints so external agents can
   discover Liberado's capabilities and hand it work. This is a new client-facing protocol
   alongside the existing HTTP/SSE chat contract — not a replacement for it (the design rule in
   `docs/spec/reference/api.md` — "no client-specific endpoints" — still applies to Liberado's *own*
   clients; A2A is a peer protocol, a different consumer class).
2. **Outbound**: Liberado *as an A2A client*, delegating a goal to a remote peer agent. This is
   conceptually a sibling to the `Provider` trait (Decision 13) — a narrow-waist abstraction for
   "hand this goal to some external reasoning system" — but for *agents*, not model providers, and
   it must go through the same capability-narrowing the dispatcher already applies to subagent
   dispatch (Decision 8), since a remote peer is strictly less trusted than an in-process subagent.

## Open questions (resolve if/when this is picked up)

- Does an inbound A2A `Task` map onto a **new conversation** (parent-linked to the requesting
  peer) or a narrower construct that skips the chat/session machinery entirely? The lineage
  fields suggest "new conversation" is the low-effort answer, but a Task's lifecycle (poll,
  cancel, artifact-typed output) doesn't fully match the chat SSE contract's shape — worth
  designing on paper before wiring it up as a client-contract variant.
- Is a remote peer agent, from Liberado's side, a `DispatchAction` variant (peer to
  `ExecuteDirect`/`DispatchSubagent`/`Clarify`/`Report` — Decision 1), or a capability reached
  *through* one of those (e.g. a peer looks like a very restricted "subagent" to the dispatcher)?
  The latter reuses more machinery; the former is more honest about trust level.
- What is the default consequence/zone posture for a request *arriving* from an external A2A
  peer? Almost certainly starts at `Clarify`/proposal-only until a specific peer is
  explicitly trusted — same fail-safe default as unlisted zones (Decision 15).
- Where does this sit in the phased roadmap (`docs/roadmap.md`)? Best guess: **not before
  Phase 3** — it is another event-source (inbound) and another externally-facing capability
  (outbound), the same category of work as vault-decoupling and cron, and shouldn't jump the
  queue ahead of the general-MCP-agent milestone (Phase 1) or self-improvement (Phase 2).

## Companion to

- `liberado-conversation-store-spec.md` — Decision 17, the seams (`author`, lineage) this leans on.
- `meshify.md` / Decision 18 — the event-bus direction this needs to be "just another
  service" rather than a bolt-on.
- `vs-hermes.md` — notes the *different* Agent Client Protocol (Zed) as a separate, lower-leverage
  gap; don't conflate the two when scoping this.
