# Liberado Conversation Store Spec

**Purpose**: Define how conversation history is persisted, traversed, and searched — and, more
importantly, fix the *seams* so the features we can already foresee (branching, parallel subagents,
debate, interruption, multi-process) are **additive** rather than rewrites. Resolves Decision 17.

**Status**: Design locked; **v1 JSONL impl landed (June 24, 2026)** — `crates/conversation-store`
(`liberado-conversation-store`): the `ConversationStore` trait + `JsonlStore` (per-conversation
JSONL, ULIDs minted at append time inside a per-conversation lock so file-order == id-order), wired
into chat via `main-agent`'s `ChatSessions` (rehydrate-per-turn, persist-on-success). The deferred
items below (persisted index, projections, SQLite/Postgres/DuckDB, branching/debate UX) remain
deferred. The only thing this spec asks the *first* line of storage code to honor is the **node
schema** (§3) — everything else (engines, indexes, projections) is swappable behind the trait (§5)
with no schema change; the v1 impl honors it.

**Last Updated**: June 24, 2026

---

## 1. Where this sits relative to the vault

Pillar 1 ("the vault is the source of truth, there is no separate database of record") is about
**knowledge** — notes, decisions, goals: the durable state the system perceives and acts on. It is
*not* a claim that every byte lives in Markdown.

Conversation history is **operational data**, the same category as the **Decision 12 runtime trace**,
and it lives where the trace lives — **append-only JSONL outside the vault Markdown** — for the
*identical* reason Decision 12 gives: high-volume, append-heavy chat writes would pollute the very
vault change-stream the daemon reacts to (the lesson that put provenance on the audit log, not
frontmatter). So:

- **The vault stays the source of truth for knowledge.** Unchanged.
- **The conversation log is the source of truth for chat**, stored outside the vault as JSONL.
- The bridge between them is a **one-way, derived projection**: a conversation can be *exported* to
  the vault as a Markdown note (§6), git-tracked and human-browsable — but that export is a *view*,
  never the system of record, and never on the live write path.

This keeps git (a terrible concurrent writer) out of the hot path while still honoring the vault
thesis for anything a human would want to read or link.

## 2. The one principle: one log, everything else is a projection

There is exactly **one thing we must not lose**: the append-only **log of message nodes**. From it,
every other structure is *rebuildable*:

| Artifact | Derived from the log | Purpose |
|---|---|---|
| Line-offset index (`Vec<u64>`) | one scan on load / mmap | O(log n) node lookup (§4) |
| Leaf-path `Vec<Message>` | walk parent pointers | what the executor actually consumes |
| Markdown-per-conversation | render the leaf path | vault export + embedding input (§6) |
| Vector index | embed the Markdown chunks | semantic recall across history (§6) |
| Recency index / conversation list | scan headers | sidebar, "resume", pagination |

Because the index, the export, and the vectors are *derived*, "storing them in parallel" is **not** a
consistency liability and **not** a real storage cost (conversation text is KB-scale; embeddings are
rebuildable). Blow any of them away and regenerate from the log.

## 3. The node schema (the only thing locked in now)

A conversation is **not** a `Vec<Message>` on disk — it is a **DAG of message nodes**. A linear chat
is the degenerate case where every node's parent is the previous leaf. Carrying `id` + `parent_id`
from day one is what makes branching / fork / loop-back / debate *additive* instead of a migration.

```rust
/// One persisted message — a node in the conversation DAG. Appended once, never mutated.
struct MessageNode {
    /// Time-sortable id, assigned AT APPEND TIME by the single writer (§7). ULID or UUIDv7.
    /// This is load-bearing: see §4.
    id: Ulid,
    /// The node this one replies to. `None` = conversation root. A `parent_id` is ALWAYS a
    /// smaller (earlier) id than `id`, so a parent is always earlier in the log.
    parent_id: Option<Ulid>,
    /// Which conversation this belongs to.
    conversation_id: Ulid,
    /// WHO produced this — not just a coarse role. `user`, an assistant model, a named subagent,
    /// a debate participant. This is the seam that makes multi-agent / debate additive.
    author: Author,
    /// user | assistant | tool (the provider-level role, distinct from `author`).
    role: Role,
    content: String,
    /// Present on assistant nodes that requested tools; on tool nodes, the id they answer.
    tool_calls: Vec<ToolInvocation>,
    tool_call_id: Option<String>,
    created_at: Timestamp,
}
```

A **conversation header** record carries lineage so subagent trees and fan-out are expressible:

```rust
struct ConversationHeader {
    id: Ulid,
    title: Option<String>,           // derived/regenerable; nice for the sidebar
    /// Set when this conversation was spawned by another (subagent dispatch, a branch promoted
    /// to its own conversation). Enables walking the agent tree.
    parent_conversation: Option<Ulid>,
    spawned_by: Option<Ulid>,        // the message node that spawned it
    created_at: Timestamp,
    updated_at: Timestamp,
}
```

The in-memory `Conversation` still hands the executor a **flat leaf-path `Vec<Message>` slice**, so
**the executor never changes** — the DAG lives only in storage and the store trait.

## 4. Traversal cost, and why sortable ids make the index nearly free

**The hot path needs no reconstruction.** A live `Conversation` already holds its current leaf path
in memory; appending a turn appends to memory and to the log together.

**Cold load is O(n), not O(n²).** Read the log once, building an in-memory `id → node` map as you
scan (you're paying O(n) anyway to load messages into context); then walking leaf→root is O(1)/hop.
No on-disk index is needed for normal loads.

**Sortable ids make random lookup O(log n) for free.** Because ids are time-sortable **and assigned
at append time by the single writer**, the log is *intrinsically sorted by id* — file order == id
order, exactly. Therefore:

- Node lookup by id = **binary search over a line-offset array** (`Vec<u64>` of line-start byte
  offsets, built in one scan on load or via mmap). That array is the *only* index, and it's a
  derived projection — often not even worth persisting.
- A `parent_id` is always *smaller*, so a parent is always *earlier* in the log: traversal only ever
  **seeks backward**.
- **Random UUIDv4 would forbid all of this** and force a real persisted `id → offset` sorted map
  maintained on every append. Choosing a sortable id is the difference between "index falls out for
  free" and "maintain a secondary structure." **This is the decision that can't be retrofitted.**

**This O(log n) lookup is not just a speed nicety — the control plane needs it.** A branch can grow
past the model's context window while the orchestration logic still has to resolve an *arbitrary*
earlier node (a fork point, a parent conversation's spawn node) that is **not** in any prompt window.
That structural random-access is a different need from "assemble a prompt," and it's exactly what
binary-search-by-id serves.

## 5. The `ConversationStore` trait (the real future-proofing)

The durable decision is the **schema (§3)** and the **trait boundary** — not the engine. The default
impl is daemonless; a networked engine is a swap-in for whoever actually needs it.

```rust
#[async_trait]
trait ConversationStore: Send + Sync {
    async fn create(&self, header: ConversationHeader) -> Result<()>;
    /// Append one completed node. Atomic; serialized per conversation (§7).
    async fn append(&self, node: MessageNode) -> Result<()>;
    /// Load the leaf path (root → `leaf`, or the conversation's current leaf if `None`).
    async fn leaf_path(&self, conversation: Ulid, leaf: Option<Ulid>) -> Result<Vec<MessageNode>>;
    /// Structural random access — the control-plane lookup of §4.
    async fn node(&self, conversation: Ulid, id: Ulid) -> Result<Option<MessageNode>>;
    /// Children of a node — the branch points.
    async fn children(&self, conversation: Ulid, id: Ulid) -> Result<Vec<Ulid>>;
    async fn list(&self, query: ConversationQuery) -> Result<Vec<ConversationHeader>>;
}
```

**Concurrency contract** (what every impl must guarantee):
- **Appends to one conversation are serialized** and atomic (no torn or interleaved writes,
  regardless of message size).
- **Different conversations are independent** — no cross-conversation contention.
- A node is persisted **only once complete** (this is what makes a cancelled turn — see the
  `turn_stream` rollback — a clean no-op on disk: nothing partial is ever written).

**Engine placement:**

| Engine | Role | When |
|---|---|---|
| **JSONL files** (one log per conversation) | Default system of record. Daemonless, grepable, crash-safe appends. | v1 |
| **SQLite** (WAL) | Drop-in graduation: one process, real index, FTS — still daemonless. Trades raw `rg` for SQL. | if/when |
| **Postgres + pgvector** | Only if we go **multi-process / multi-tenant**; folds the vector store into the same DB. | if ever |
| **DuckDB** | *Not* a store (OLAP, weak at row-at-a-time appends). A great **analytics sidecar** pointed straight at the JSONL ("tokens/week", "tools used"). | optional |
| **sqlite-vec** | Daemonless vector index if we stay on files/SQLite. | with §6 |

**Why daemonless is the default, on purpose:** a background-daemon DB is *anti-modular* — it makes
every crate that touches storage depend on a running server, which is friction at every composition.
An embedded default keeps the storage crate self-contained, so the crate set still glues into a
human-chat product *or* an autonomous agent (the substrate goal) without dragging Postgres along. The
trait is what lets the rare multi-process deployer opt into Postgres.

## 6. Projections for the long-conversation case

A genuinely long agentic run (tens of thousands of nodes) can't be fed to a model wholesale — the
context window is the ceiling long before traversal cost is. So the long case is a **retrieval**
problem, not a faster-pointer-walk problem, and it's served by *projections*, all rebuildable from
the log:

- **Markdown-per-conversation** — render the leaf path to a clean Markdown doc. Doubles as the
  **vault export** (git-tracked, human-readable; honors Pillar 1 as a *view*) and the **embedding
  input** (Markdown chunks cleanly).
- **Vector index** (`sqlite-vec` now / `pgvector` if Postgres) — semantic recall across history,
  keyed by node/conversation id. Embeddings are derived data, never source of truth.
- **Recency index** — "recent N + relevant M" is the slice you actually load for a long run.

These are **deferred**; §3's id choice is the only thing they require to exist now.

## 7. Concurrency model: the conversation as a single-writer actor

The clean way to satisfy §5's serialization contract — and to make every foreseen feature natural —
is to model each open conversation as an **actor that owns its log**. Participants don't write the
file; they **send messages to the conversation**:

- **User, assistant, subagents, debate participants** are all just senders.
- The actor serializes appends (so a fat tool-result line can't tear against a concurrent append —
  the one place raw `O_APPEND` stops being atomic).
- **Interruption is a control message** to the actor — the generalization of the stream cancel
  primitive already built (`turn_stream`'s drop-and-rollback).
- **Branching** is "append a node whose parent is an older node"; **loop-back-to-fork** is "make a
  new leaf from an old node" — both fall out of append-only + parent pointers.

(SQLite's WAL would also serialize these appends for free — its one real edge over raw JSONL — at the
cost of raw `rg`. The actor keeps both grep and safe concurrency, behind the same trait.)

## 8. Deferred feature → enabling seam

We are **not building these now.** The point of this spec is that each is *additive* because its seam
exists:

| Future feature | Seam that makes it additive |
|---|---|
| Branching / loop back to a fork | Nodes are a DAG (`id` + `parent_id`), linear is the degenerate case (§3) |
| Parallel subagent dispatch | `ConversationHeader` lineage (`parent_conversation`, `spawned_by`) (§3) |
| Parallel conversations (fan-out) | Conversations are independent logs (§5) |
| Debate / multi-agent | `author` identity on every node (§3) |
| User interrupts a debate | Single-writer actor + control message (§7) |
| Resolve a node beyond the context window | Sortable-id O(log n) lookup (§4) |
| Multi-process / multi-tenant | `ConversationStore` trait → Postgres impl (§5) |
| Semantic search over history | Markdown + vector projections (§6) |

## 9. What is decided now vs deferred

**Decided / honor immediately when storage is first written:**
1. Conversation history is JSONL **outside the vault** (operational data; Decision 12 category).
2. Storage is an **append-only log of `MessageNode`s** with the §3 schema — a **DAG**, not a list.
3. Node ids are **time-sortable (ULID/UUIDv7), assigned at append time** (§4). *Non-negotiable; the
   one thing that can't be retrofitted.*
4. All access goes through the **`ConversationStore` trait**; the v1 impl is JSONL.
5. Per-conversation **single-writer** serialization; nodes persisted **only when complete**.

**Deferred (additive later, no schema change):**
- The persisted line-offset index (in-memory on load is fine until profiled otherwise).
- Markdown/vault export, vector index, recency index (§6).
- SQLite / Postgres / DuckDB impls (§5).
- Branching/debate/parallel UX and orchestration (§8) — the engine just won't fight them.

---

## Companion to

- **Decision 12** (`liberado-architecture-decisions.md`) — append-only JSONL outside the vault; this
  spec is the conversation-shaped instance of the same rule.
- **Decision 17** (`liberado-architecture-decisions.md`) — the log entry this spec resolves.
- `docs/reference/api.md` — the chat API/SSE contract; "Sessions / Persistence" on its roadmap is this
  store surfacing through the API.
