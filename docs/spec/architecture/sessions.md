# Sessions

**Status**: living architecture. Landed 2026-07-12/13 (session-focus slices S1–S7, S5′, forking).
**Related**: [`agentic-loops.md`](agentic-loops.md) (kernel vs packs) ·
[`channels-and-interactivity.md`](channels-and-interactivity.md) (the three channels; interactivity
as a capability) · [`contracts.md`](contracts.md) · [`../reference/api.md`](../../spec/reference/api.md) ·
[`../roadmap/archive/session-focus-plan.md`](../../future-work/archive/session-focus-plan.md) (how it was built, slice by
slice — history, not the model itself)

---

## The one idea

**Everything is a `Session`.** A chat with the main agent, a `/spawn`ed coding session, a cron
firing, a `delegate`d subagent — one type, one store, one id space, one list.

They differ by **attributes, not subtypes**:

| Attribute | Meaning |
|---|---|
| `goal: Option<GoalSpec>` | **The** distinguishing one. `Some` ⇒ runs to a terminal status. `None` ⇒ a chat, which is simply *open*. |
| `visibility: Foreground \| Background` | Was a human watching when it started? A cron/webhook/subagent was not. |
| `grant: SessionGrant` | What it is allowed to do (capabilities), which profile it came from, and the pack's opaque overrides. Resolved once, **never widened**. |
| `parent_session` / `spawned_by` | A real `Ulid` edge to the session (and the node) it came from. The session tree is walkable. |

> **Terminality *is* `goal.is_some()`.** That single `Option` is the entire difference between a
> conversation and a "goal session". It is why there is one store and one endpoint, and why the
> switcher branches on `has_goal()` rather than on a row type it invented.

This was not always true, and the vocabulary still leaks: "conversation" in older prose (and in the
`ConversationStore` trait name) means *a session viewed through the chat lens*. There is no such
thing as a conversation that is not a session.

## One store, two lenses

`liberado-session-store::SessionStore` is the converged store. One directory
(`liberado_config::sessions_dir()`), one append-only JSONL log **per session**:

```
line 0   header   the SessionHeader (rewritten on title change; replay takes the last one)
         node     a MessageNode  — a provider-replayable turn, carrying parent_id (the DAG)
         event    a SessionEvent — an observation from a pack (tool started, awaiting input, …)
         status / finish        — lifecycle transitions, for goal-bearing sessions
```

A chat's log is all `node` lines. A goal session emits **both** — and that is the case which proves
these were never two different things.

### Turns vs events — the distinction a pack has to get right

* An **event** ([`SessionEvent`]) is an *observation*: a tool started, a role finished, the pack is
  awaiting input. Something happened. It is what a live subscriber watches.
* A **turn** (`PackContext::record_turn`, `SessionRecordStore::append_turn`) is *dialogue*: a
  clarifying question, the human's answer, a drafted contract, the final summary. Something was
  **said**. It becomes a node in the message DAG.

Packs recorded only events until 2026-07-13, and it cost two things that looked like separate
features until you notice they are the same gap: a coding session's intake Q&A was **not searchable**
(`chat-search` matches message nodes, and there were none), and a goal session **could not be forked**
(forking copies a node prefix, and a flat event log has no `parent_id`).

The kernel records the turns no pack should be able to forget — the goal opens the transcript as the
human's first turn, whatever a human sends in is a turn by definition, and the outcome closes it. A
pack adds its own dialogue on top. A goal session now reads as what it is:

```
user       capture a note about pack turns      ← the goal
assistant  What should I title the note?        ← the pack asked
user       Turn Recording Works                 ← you answered
assistant  wrote note titled 'Turn Recording Works'   ← the outcome
```

It implements **both** store traits:

| Lens | Trait | Sees | Home |
|---|---|---|---|
| chat | `ConversationStore` | every session (a goal session has a transcript too) | `liberado-conversation-store` |
| kernel | `SessionRecordStore` | only goal-bearing ones — `GoalSessionRecord` *cannot represent* a goal-less session | `liberado-session` |

Two lenses is not a compromise, it is a **layer rule doing its job**: the session kernel may not
depend on `liberado-provider`, so it cannot know what a `Message` is. A store holding both must
therefore live *above* the kernel and expose a kernel-shaped view downward. The duplication that
convergence removed was in the **storage**; what remains is two typed views of one log.

### The append invariants (all three are load-bearing, and all three were broken)

1. **Ids are minted monotonically.** `leaf_path(conv, None)` finds the newest turn by taking the
   largest id, so two appends inside the same millisecond (an assistant node and its tool-result
   node) must not invert. `Ulid::new()` does not guarantee that; `ulid::Generator` does.
2. **The write happens under the same lock the id was minted under**, so file order == id order.
   Minting under the in-memory lock is not enough: the durable write lands after that lock is
   dropped, so two appends can mint `id1 < id2` and then race.
3. **One line, one `write_all`.** `writeln!` goes through `write_fmt` and may issue several `write`
   syscalls, letting two appenders interleave *within* a line — and a single spliced line fails
   replay for the **whole session**.

The conformance suite for all of this lives in `crates/session-store/tests/conversation_lens.rs`,
and it runs against `SessionStore` — i.e. against the store production actually uses. That sounds
obvious; it was not the case until 2026-07-13, and all three defects above survived precisely
because the suite had been pointed at a store nothing ran on.

## Background sessions — hosted on the one engine

A cron firing, a webhook/hook, and a `delegate`d subagent are **hosted** `Visibility::Background`
sessions on the same `GoalSessionHub` as `/spawn`, via the `dispatch` domain pack
(`liberado-dispatch-pack`). They are joinable and cancellable; `domain: "dispatch"` is a registered
pack, not a fake.

| Entry | Pack | Notes |
|---|---|---|
| `/spawn`, `POST /api/goals` | coding / life / … | Foreground when a human starts them |
| cron / webhook / vault reaction | `dispatch` | `ReactionOutcome::Dispatched { session_id }` |
| face-agent `delegate` | `dispatch` | Awaits terminal inside the chat turn; no `AskHuman` (D-e) |

See [`../roadmap/archive/one-execution-engine-plan.md`](../../future-work/archive/one-execution-engine-plan.md) for the
convergence (E1–E7).

## Authority (S6)

`SessionGrant { capabilities, profile, overrides }` is resolved by the **server** from
`[[session_profiles]]` in `topology.toml` + `[[grants]]` in `policy.toml`, recorded on the session,
and **never widened** thereafter. The kernel never reads config — it is handed an already-resolved
authority, never a key to look up.

`Capability::AskHuman` is what makes interactivity a *capability* rather than a subtype
([`channels-and-interactivity.md`](channels-and-interactivity.md), Decision A). A grant without it
gets a **closed input channel**, so the pack physically cannot block on a human, and
`POST /api/goals/{id}/message` answers **403** (never allowed) — distinct from **409** (too late).

Note that `AskHuman` and `visibility` are independent on purpose: a background session *may* hold
`AskHuman` (it would await an answer nobody is there to give, until its idle budget kills it), and a
foreground one may lack it.

## Forking

`POST /api/sessions/{id}/fork` — branch a conversation, keeping the original. In the TUI: `/fork`
(the whole thing), `/fork <n>` (through your turn *n*), or **`f` while browsing history** (fork at the
message under the cursor).

**Copy, not reference.** The prefix nodes are copied into the fork's own log with fresh ids,
re-parented onto each other — not stitched in from the parent at read time. Two reasons:

1. It preserves the store's one real invariant: **a session's log is self-contained.** Line 0 is its
   header and every node it needs is in the file — which is what makes it greppable on its own
   (`chat-search` reads these files directly), replayable on its own, and deletable without gutting
   some other session.
2. It gives **snapshot** semantics, which is what a fork *means*: continue the original afterwards
   and the fork does not move.

Lineage (`parent_session`, `spawned_by`) is still recorded, so the tree stays walkable even though
the content stands alone.

The branch point is named by **turn**, not by node id — the server resolves turn → node; the store
speaks node ids. That is not a shortcut: a *live-streamed* message never receives a node id from the
SSE stream, so if the branch point had to be a node, every message not reloaded from the server would
be unforkable. Turn counting works identically for live and rehydrated messages.

A goal session **can** be forked — its dialogue is turns now (see above), so it has a node prefix to
branch from. Forking a coding session at its freeze point (contract A vs contract B) is the valuable
version of forking, and this is what makes it representable. A fork is always a *chat*: it inherits
the transcript, not the goal, because a goal session that no pack is running is not honestly
"running toward" anything. A session in which nothing was said is still refused (400).

## The API surface

| Endpoint | What it is |
|---|---|
| `GET /api/sessions` | **The** list. Every session, newest first. Read this. |
| `POST /api/sessions/{id}/fork` | Branch, keeping the original. |
| `GET /api/conversations[/{id}]` | The **chat lens** — every session, titled by its goal if it has one. |
| `GET /api/goals[/{id}]` | The **kernel lens** — only goal-bearing sessions. |

The two older endpoints are not legacy: they are the two lenses, and a caller often legitimately wants
exactly one of them. See [`../reference/api.md`](../../spec/reference/api.md).

## What a surface owes the user

The client-side half of this model — what any surface showing sessions must display and let you do —
is [`session-surface-contract.md`](session-surface-contract.md). It is derived from the TUI (already a
complete session client) rather than invented, and exists so the WebUI does not have to be
reverse-engineered out of `crates/tui/`.

## Where the code lives

| Crate | Role | Holds |
|---|---|---|
| `liberado-session` | kernel | `GoalSpec`, `SessionGrant`, `Visibility`, `SessionEvent`, `GoalSessionHub`, `DomainPackRunner`, and the `SessionRecordStore` seam |
| `liberado-dispatch-pack` | pack | `DispatchPack` — the dispatcher + orchestrator as a pack, so cron/webhook/`delegate` are hosted sessions (there is no `BackgroundRun` any more; it was the seam that existed *because* those ran outside the hub) |
| `liberado-session-store` | store | `SessionStore` — the converged engine; `SessionHeader`; both lens impls |
| `liberado-conversation-store` | store | the `ConversationStore` **trait** + the message-node DAG types. No implementation — `SessionStore` is the only one |
| `chat-client-contract` | client | `SessionSummary`, `SessionKind`, `VisibilityWire`, `ForkRequest`/`ForkResponse` |
