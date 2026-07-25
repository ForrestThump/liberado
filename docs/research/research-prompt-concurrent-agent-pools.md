# Research prompt: proven architectures for concurrent, multi-authority agent systems

Copy everything below this line into a research-capable model (Grok, or similar with good web
access) as-is.

---

## Context

I'm the architect of a personal, self-hosted "life OS" agent system — a single long-running Rust
daemon that watches for triggers (file changes, cron schedules, incoming webhooks) and reasons
about them with an LLM, then acts through a set of tools (via the Model Context Protocol, MCP).
Safety is engineered, not prompted: every action goes through a hard capability/permission
boundary that the LLM can only ever *narrow*, never widen — an agent can be handed a subset of the
system's authority, but it can never grant itself more than it started with. High-consequence or
ambiguous actions get downgraded to "propose and wait for human approval" rather than executing
directly.

**Current architecture (the part that's well-understood and already built):** one central
"dispatcher" classifies each incoming goal and routes it to one of: execute directly, ask a
clarifying question, hand off to a subagent, or propose-and-wait-for-approval. When it hands off to
a subagent, that subagent runs *concurrently* with others (today, as in-process async tasks with a
concurrency cap), but every subagent's authority is a *narrowed slice* of the same parent
dispatcher's authority — there's one delegator handing out restricted mandates to disposable
workers, and the workers don't have independent standing authority of their own, don't persist
beyond their task, and don't talk to each other directly. This is the classic
orchestrator/supervisor-and-workers pattern and it works fine for what it is.

**What I'm about to build next (well-scoped, not the research question):** splitting the *single*
dispatcher into multiple independently-configured "pools" — each with its own model, tuning, and
capability grant, selected by which trigger produced the goal (e.g., a cron-triggered goal might
route through a deliberately more restricted pool than an interactive chat goal). This is closer to
running several independent, differently-scoped services side by side than it is to a
multi-agent-coordination problem, and I already have a design for it.

## The actual open question

What I *don't* have a design for, and suspect is a genuinely different and less mature problem, is
this: what happens once there are **multiple agents that are independently authoritative** (not
disposable workers under one delegator's narrowed mandate, but agents that each have their own
standing scope of responsibility) **running concurrently and potentially needing to coordinate** —
sharing state, avoiding duplicate or conflicting work, communicating results to each other, maybe
even negotiating who does what. This feels like a step change in complexity from "one delegator,
many disposable narrowly-scoped workers," and I want to know what's actually been proven to work
before I design anything in this space, rather than reinventing something known-bad from first
principles.

I'm also tracking the emerging **Agent2Agent (A2A)** protocol (Linux Foundation, originally
Google) — an open spec for cross-vendor agent interop (capability discovery via an "AgentCard,"
a task lifecycle of submit/poll/stream/cancel) — as a later integration target once the
multi-authority-pool foundation exists, since a remote peer agent is conceptually "yet another
independently-authoritative agent this system might coordinate with," just over the network
instead of in-process.

## What I want you to research

1. **Named architectural paradigms for multi-agent coordination**, with enough specificity that
   I could look each one up and read further — not just "multi-agent systems" as a category, but
   the actual named patterns: e.g., blackboard architectures, contract-net protocol, hierarchical
   supervisor trees (and how that differs from what I already have), market-based/auction task
   allocation, publish-subscribe/event-driven coordination, actor-model supervision (Erlang/OTP
   style) applied to LLM agents, peer-to-peer negotiation protocols, graph-based multi-agent
   orchestration (e.g., what LangGraph's multi-agent graphs or similar frameworks actually do
   under the hood), swarm/group-chat patterns (e.g., AutoGen's group chat, CrewAI's crew/process
   model, OpenAI's Swarm/Agents SDK handoff model, Microsoft Semantic Kernel's agent patterns).
   For each: what problem was it actually designed to solve, and does that match "several
   independently-authoritative LLM agents that need to avoid stepping on each other and
   sometimes hand off or share work"?

2. **How production and research systems handle authority/permission boundaries *between*
   concurrently active agents** — not the single-supervisor-narrows-a-subagent's-scope pattern
   (I already have that), but cases where two or more agents each have their own standing
   authority and something has to prevent them from conflicting, duplicating effort, or one
   silently expanding into the other's territory. Real examples wanted, not just theory.

3. **How they handle concurrent access to shared, mutable state** across independently-acting
   agents — race conditions, conflict resolution, optimistic vs. pessimistic concurrency, event
   sourcing / CRDTs / other approaches — specifically in the context of *agentic* systems (not
   generic distributed-systems theory, though pointers to the distributed-systems roots of
   whatever pattern is relevant are welcome).

4. **Known failure modes and anti-patterns** in multi-agent coordination — where has this been
   tried and shown to *not* work well, or to add coordination overhead that exceeded the benefit?
   I'd rather know what to avoid than discover it the hard way.

5. **The current state of the Agent2Agent (A2A) protocol** specifically — how mature is it
   really (adoption, production usage, spec stability), and does its design assume or imply any
   particular multi-agent coordination model on the *receiving* agent's side (i.e., does A2A
   itself have opinions about how a system should internally handle "I just got handed a task by
   an external peer while also doing my own independent work")?

6. **Recent (2024-2026) industry writing on multi-agent architecture from major labs/vendors**
   building agent products — Anthropic, OpenAI, Google, Microsoft, LangChain/LangGraph,
   CrewAI, and similar — especially anything that discusses *when* to reach for true
   multi-agent coordination versus when a single-orchestrator/subagent-workers pattern (what I
   already have) is deliberately the better, simpler choice. I want the counter-argument too, not
   just "more agents is more powerful."

## Constraints on what would actually be usable to me

- Single-user, self-hosted, homelab scale — not designing for enterprise multi-tenant load. Simpler
  and more inspectable beats more scalable.
- Written in Rust; the eventual answer needs to be something implementable as Rust
  types/traits/processes, not a Python-framework-specific abstraction I'd have to reinvent from a
  paper.
- The capability/permission containment property (agents can only ever be handed a *subset* of
  authority, never grant themselves more) is non-negotiable and must survive whatever coordination
  model gets adopted — if a paradigm requires agents to trust each other with broader authority to
  function, flag that explicitly as a mismatch rather than glossing over it.
- I'd rather adopt a proven, boring pattern than a novel one — this is a personal system I need to
  actually trust, not a research showcase.

## What I want back

- A ranked short-list of paradigms that plausibly fit the constraints above, each with: what it is,
  where it's actually been used in production (not just described in a paper), and its known
  failure modes.
- Explicit call-outs of anything that's a poor fit given the constraints (and why), so I don't
  waste time chasing something that only makes sense at larger scale or with different trust
  assumptions.
- Citations/links wherever possible so I can go read primary sources myself.
- If you genuinely think "you don't need true multi-agent coordination yet, the
  supervisor/narrowed-subagent pattern you already have covers your actual use case, only add
  coordination once you have a concrete case it doesn't" — say that plainly. I want an honest
  answer, not validation of the premise that I need this.
