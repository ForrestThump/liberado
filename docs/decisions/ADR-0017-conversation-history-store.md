---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0017
open_items: false
---

# ADR-0017: Conversation History Store

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0017 (`conversation-history-store`)

## Context

The chat agent (`main-agent`) holds conversation history in memory only — it is
lost on restart and exists as a single session. How we persist it is load-bearing *not* for v1 chat
but for everything the vision wants next: conversation **branching**, **parallel subagent dispatch**,

## Decision

**An append-only log of message *nodes*, JSONL outside the vault, behind a
`ConversationStore` trait.** Key points:
- **Operational data, not vault knowledge.** Conversation history is the **same category as the
  Decision 12 runtime trace** — append-only JSONL *outside* the vault Markdown, for the identical
  reason: high-volume chat writes would pollute the change-stream the daemon reacts to. Pillar 1
  ("vault is source of truth") is about *knowledge*; it is not a claim that chat logs are notes. The
  vault bridge is a **one-way derived Markdown export** (a view, git-tracked, human/vector-friendly),
  never the system of record and never on the live write path.

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-conversation-store-spec.md`
- `architecture/overview.md`
- `architecture/positioning.md`
- `architecture/modularity.md`
- `../future-work/ideas/archive/meshify.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: The chat agent (`main-agent`) holds conversation history in memory only — it is
lost on restart and exists as a single session. How we persist it is load-bearing *not* for v1 chat
but for everything the vision wants next: conversation **branching**, **parallel subagent dispatch**,
**fan-out conversations**, **debate** systems, and user **interruption**. Pick the wrong storage
*shape* now (a flat list, random ids, a mandatory DB daemon) and those become rewrites; pick the
right *seams* and they become additive.

**Open questions**:
- Does conversation history live in the vault (Pillar 1) or outside it?
- Linear list or a branchable structure on disk?
- What engine — JSON/JSONL, SQLite, Postgres, DuckDB — and does "future-proof for concurrent users"
  force a networked DB?
- How is it searched (grep vs FTS vs vectors)?

**Status**: Complete (full design in `liberado-conversation-store-spec.md`).

Decision 17: **An append-only log of message *nodes*, JSONL outside the vault, behind a
`ConversationStore` trait.** Key points:
- **Operational data, not vault knowledge.** Conversation history is the **same category as the
  Decision 12 runtime trace** — append-only JSONL *outside* the vault Markdown, for the identical
  reason: high-volume chat writes would pollute the change-stream the daemon reacts to. Pillar 1
  ("vault is source of truth") is about *knowledge*; it is not a claim that chat logs are notes. The
  vault bridge is a **one-way derived Markdown export** (a view, git-tracked, human/vector-friendly),
  never the system of record and never on the live write path.

  > **Clarifying note (2026-06-26 — do not rewrite history):** the matured pillars demote
  > "the vault is the source of truth." **The vault (TurboVault) is now the default, privileged
  > perception+storage plugin, not a hard dependency.** The core (dispatch / execute / MCP runtime /
  > chat / conversation-store) is vault-agnostic; the vault's coupling is isolated to the reactive
  > subsystem (watch + provenance loop-breaking), which becomes the vault plugin behind an
  > event-source trait (Decisions 18, 19). See the three pillars in
  > [`architecture/overview.md`](../spec/architecture/overview.md) and
  > [`architecture/positioning.md`](../spec/architecture/positioning.md). Wherever an earlier
  > decision in this log says "the vault is the source of truth," read it through this clarification.
- **One log, everything else is a rebuildable projection** — the line-offset index, the leaf-path
  slice the executor consumes, the Markdown export, the vector index, the recency/list index are all
  *derived from* the log. So parallel storage is neither a consistency liability nor a real cost.
- **Messages are a DAG (`id` + `parent_id`), not a `Vec`.** Linear chat is the degenerate case. This
  is the seam that makes branching / loop-back / debate additive. The executor still sees a flat
  leaf-path slice, so it never changes. Conversation headers carry **lineage**
  (`parent_conversation`, `spawned_by`) for subagent trees; nodes carry an **`author`** identity (not
  just user/assistant/tool) for multi-agent/debate.
- **Node ids are time-sortable (ULID/UUIDv7), assigned at append time.** *This is the one choice that
  can't be retrofitted.* It makes the log intrinsically id-sorted, so random node lookup is
  O(log n) binary-search over a line-offset array (parents always earlier ? seek backward only), with
  no persisted secondary index. Random UUIDv4 would force a real maintained index. The control plane
  genuinely needs this lookup: a branch can outgrow the context window while orchestration must still
  resolve an arbitrary earlier fork node.
- **Daemonless by default, on purpose.** The v1 impl is JSONL; **SQLite (WAL)** is a drop-in
  graduation (one process, real index/FTS, still daemonless); **Postgres + pgvector** is a swap-in
  *only if* we ever go multi-process/multi-tenant (it also folds in vectors); **DuckDB** is an
  analytics sidecar over the JSONL, not a store. A background-daemon DB is *anti-modular* — it would
  drag a running server into every composition of the crate set, against the "glue into LibreChat or
  an autonomous agent" substrate goal. The trait lets the rare multi-process deployer opt into
  Postgres without touching the agent loop.
- **Concurrency = per-conversation single-writer actor.** Participants (user, subagents, debaters)
  *send* to the conversation; the actor serializes appends (safe regardless of line size) and
  persists a node **only when complete** (so a cancelled streaming turn is a clean no-op on disk —
  the `turn_stream` rollback stays purely in-memory). Interruption is a control message — the
  generalization of the stream-cancel primitive already built. Different conversations are
  independent logs (no contention), which is the *only* "concurrency" the foreseen features actually
  need — none of them require multiple OS processes.
- **Search**: at API-request scale, ripgrep over JSONL is functionally equivalent to a DB index, so
  search performance gets zero weight; the long-conversation case is a *retrieval* problem (vector +
  recency projections), not a faster-traversal problem.

**Decided now**: JSONL-outside-vault; append-only log of DAG nodes; **sortable ids assigned at
append**; the `ConversationStore` trait; single-writer-per-conversation. **Deferred (additive, no
schema change)**: the persisted index, the Markdown/vector/recency projections, SQLite/Postgres/
DuckDB impls, and the branching/debate/parallel UX.

---

## Tier 1 (matured vision, 2026-06-26): Modularity & Mesh

These two decisions record the matured architectural vision agreed in the 2026-06-26 planning
session. They are load-bearing because they reframe the substrate (event-driven, vault-optional) that
every later feature builds on. See the three pillars in
[`architecture/overview.md`](../spec/architecture/overview.md), the thesis in
[`architecture/positioning.md`](../spec/architecture/positioning.md), the seam plan in
[`architecture/modularity.md`](../spec/architecture/modularity.md), and the mesh source in
[`docs/ideas/archive/meshify.md`](../future-work/ideas/archive/meshify.md).
