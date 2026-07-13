# Sessions

**Status**: living architecture. Landed 2026-07-12/13 (session-focus slices S1–S7, S5′, forking).
**Related**: [`agentic-loops.md`](agentic-loops.md) (kernel vs packs) ·
[`channels-and-interactivity.md`](channels-and-interactivity.md) (the three channels; interactivity
as a capability) · [`contracts.md`](contracts.md) · [`../reference/api.md`](../reference/api.md) ·
[`../roadmap/session-focus-plan.md`](../roadmap/session-focus-plan.md) (how it was built, slice by
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

A chat's log is all `node` lines. A goal session's is mostly `event` lines. **An interactive coding
session (S7) emits both** — its intake Q&A are turns, its tool calls are observations — and that is
the case which proves these were never two different things.

It implements **both** store traits:

| Lens | Trait | Sees | Home |
|---|---|---|---|
| chat | `ConversationStore` | every session (a goal session has a transcript too) | `liberado-conversation-store` |
| kernel | `SessionRecordStore` | only goal-bearing ones — `GoalSessionRecord` *cannot represent* a goal-less session | `liberado-session` |

Two lenses is not a compromise, it is a **layer rule doing its job**: the session kernel may not
depend on `liberado-provider`, so it cannot know what a `Message` is. A store holding both must
therefore live *above* the kernel and expose a kernel-shaped view downward. The duplication that
convergence removed was in the **storage**; what remains is two typed views of one log.

**Ids are minted monotonically.** `leaf_path(conv, None)` finds the newest turn by taking the largest
id, so two appends inside the same millisecond (an assistant node and its tool-result node) must not
invert. `Ulid::new()` does not guarantee that; `ulid::Generator` does.

## Background sessions — recorded, not hosted

A cron firing, a webhook/hook, and a `delegate`d subagent are recorded as `Visibility::Background`
sessions (`liberado_session::BackgroundRun`), in the same store and the same list as your chats.
Before this, they fired into the void: a model was called, your vault was possibly written to, and
the only trace was a log line.

**They are recorded, not hosted.** The dispatcher/orchestrator still executes them; the
`GoalSessionHub` and its `DomainPackRunner` packs are *not involved*. So joining one is **read-only**
— you watch what it did, you do not steer it.

> ### The seam this leaves open
>
> There are, honestly, **two execution engines**:
>
> | Engine | Runs | Reached by |
> |---|---|---|
> | `GoalSessionHub` + `DomainPackRunner` packs | goal sessions (coding, life) | `/spawn`, `POST /api/goals` |
> | dispatcher + orchestrator | daemon reactions, `delegate` | cron, webhooks, vault changes, the face agent's `delegate` tool |
>
> D7 unified how sessions are *stored and displayed*. It did not unify how they are *run*. A
> background session's `domain` is therefore recorded as `dispatch` — not `coding` or `life` — because
> claiming a pack ran it would be a lie a surface would then act on (it would offer to steer it).
>
> Routing unattended triggers through the hub as real packs is a later, larger convergence. This is
> the largest remaining piece of structural debt in the session model, and it is deliberate: the
> visibility was worth having before the convergence was.

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

A goal session records **events, not turns**, so forking one is refused (400) rather than handing back
an empty conversation that looks like it worked.

## The API surface

| Endpoint | What it is |
|---|---|
| `GET /api/sessions` | **The** list. Every session, newest first. Read this. |
| `POST /api/sessions/{id}/fork` | Branch, keeping the original. |
| `GET /api/conversations[/{id}]` | The **chat lens** — every session, titled by its goal if it has one. |
| `GET /api/goals[/{id}]` | The **kernel lens** — only goal-bearing sessions. |

The two older endpoints are not legacy: they are the two lenses, and a caller often legitimately wants
exactly one of them. See [`../reference/api.md`](../reference/api.md).

## Where the code lives

| Crate | Role | Holds |
|---|---|---|
| `liberado-session` | kernel | `GoalSpec`, `SessionGrant`, `Visibility`, `SessionEvent`, `GoalSessionHub`, `DomainPackRunner`, `BackgroundRun`, and the `SessionRecordStore` seam |
| `liberado-session-store` | store | `SessionStore` — the converged engine; `SessionHeader`; both lens impls |
| `liberado-conversation-store` | store | the `ConversationStore` trait + the message-node DAG types. Its own `JsonlStore` is the pre-convergence implementation, now used only by tests |
| `chat-client-contract` | client | `SessionSummary`, `SessionKind`, `VisibilityWire`, `ForkRequest`/`ForkResponse` |
