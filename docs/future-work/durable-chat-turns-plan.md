# Durable chat turns — a turn should outlive the connection watching it

**Status**: **built and verified live** (2026-08-02, `b2eeec7`). Written 2026-08-01 after a refresh
during a delegated turn discarded a reply the daemon went on to produce successfully 93 seconds
later.

Live proof: a client killed 12 seconds into a delegating turn, which then ran **205 seconds**
unwatched and persisted its whole transcript — including a delegation that failed, was noticed, and
was retried to success. Every second past the 12s mark is work the old code discarded.

**Wanted**: refreshing the page, switching apps, losing signal, or closing a tab must not kill a
running turn or discard its answer. On return, the conversation shows what happened. This holds for
any turn — delegating or not, profiled or not.

## Why this is not a flag

Three separate things are currently tied to the HTTP connection, and fixing one without the others
makes the system worse rather than better.

| # | Coupling | Where | What breaks if it alone is changed |
|---|---|---|---|
| 1 | **Lifetime** — the turn runs inside `tokio::select!` racing `tx.closed()`, so a disconnect drops the turn future, cancelling inference and rolling back | `api/chat.rs`, `chat_stream_core` | — |
| 2 | **Transport** — events go to an `mpsc::channel(64)` created per request. Nothing outside that one response can observe the turn | same | Decouple lifetime only: the turn survives but nobody can ever see it. Silent success. |
| 3 | **Cancellation** — closing the stream *is* the stop button. `stop_stream()` in the WebUI calls `EventSource.close()` and nothing else; there is no chat cancel endpoint | `webui/components/chat.rs:318`, `close_current_stream` | Decouple lifetime only: the stop button stops stopping anything, and a runaway turn cannot be halted at all. |

(3) is the one that gets missed. It is the reason this cannot be staged as "just don't cancel on
disconnect" — that single change silently removes the only way to stop a turn.

## The good news: half of it already exists

Goal sessions solved this problem already, and the machinery is not parallel infrastructure — it is
*the same store*.

`session-store/src/jsonl.rs` holds one entry type for both lenses:

```rust
struct Live {
    header,
    nodes: Vec<...>,   // chat's message tree
    events: Vec<SessionEvent>,
    bus: broadcast::Sender<SessionEvent>,
}
```

Every conversation already has an `events` vec and a `bus`. `subscribe(id)` returns
`(history, broadcast_rx)` and would work on a chat id today — it would just return an empty history
and a bus nobody sends to. **The chat turn writes to a per-connection mpsc instead of the session's
own bus. That is the whole gap.**

And `GoalSessionHub::start` already demonstrates the target shape: `tokio::spawn(run_session(…))`,
detached, with events pushed through the store. That is exactly why, on 2026-08-01, the *subagent*
survived the disconnect and ran to completion while the chat turn that spawned it was cancelled.

This is the D7 unified-session convergence, at the runner layer. The surface converged first, the
store is already shared; this is the step that was deferred.

## Design

**Host the chat turn the way a goal session is hosted.**

1. **Run detached.** The turn is spawned and owned by the chat sessions layer, not by the request. A
   dropped response no longer touches it.
2. **Publish to the session's bus**, not a per-request channel. `AgentEvent` already maps onto the
   converged vocabulary for SSE (`to_sse`); it needs to reach `push_event` instead of an mpsc.
3. **`/api/chat/stream` becomes start-or-attach.** With a turn already running for the conversation,
   the request attaches to it. Without one, it starts a turn and attaches. Keyed on conversation id,
   which also makes a double-send on reconnect idempotent rather than a second inference.
4. **Explicit cancel** — the chat equivalent of `POST /api/goals/{id}/cancel`, wired to the stop
   button, replacing "close the EventSource" as the cancellation mechanism.
5. **Persist on completion, regardless of audience.** The reply lands because the turn finished, not
   because someone was watching. This is the actual ask.

## The parts that are easy to get wrong

**Tokens are the awkward case.** Persisting every token delta into the JSONL would bloat it badly —
goal sessions emit far fewer events than a streaming chat does. But a client that reattaches
mid-answer needs the text so far, or it stares at nothing until the turn ends. Suggested split:
tokens ride the bus but are not persisted, and the running turn keeps its partial reply in memory so
a reattaching client gets one catch-up chunk. The durable record still only gains the completed
message, exactly as today.

**Daemon restart mid-turn.** Today this is invisible because the connection dies with the process.
Detached turns need an answer: goal sessions park (`SessionStatus::Parked`, E6). A chat turn probably
cannot resume — inference is not replayable from where it stopped — so the honest outcome is marking
it abandoned so the UI shows a turn that died rather than one that is still thinking. **This must not
silently look like a running turn**, or it reintroduces the hang it was meant to fix.

**Incognito.** The ghost teardown assumes the connection owns the RAM-only session's lifetime
(`opened_incognito`, `discard_ghost`, the `pagehide` mirror). Detached turns break that assumption:
discarding a ghost while a turn is still writing to it needs a defined order. This is the same class
of bug that once deleted a saved conversation, so it deserves its own tests rather than a note.

**Two tabs.** Broadcast makes this work by construction, but it is now reachable — two clients on one
conversation will both receive the stream. Worth asserting rather than assuming.

**The stop button's meaning changes.** It currently means "stop showing me this". It will mean "stop
doing this". Those differ when a delegation is in flight, and the second is what people expect.

## Sequencing

1. **Explicit chat cancel first**, while disconnect still cancels. Additive, independently useful,
   and it means step 2 does not create a window with no way to stop a turn.
2. **Publish turn events to the session bus** — still cancelling on disconnect. Now observable from
   more than one place, which is what makes the rest testable.
3. **Detach the turn's lifetime** and make the endpoint start-or-attach.
4. **Reattach catch-up** (partial reply buffer), then incognito ordering, then restart semantics.

Each step is shippable. Only 3 changes behaviour the user will notice.

## Testable claims

Ground truth, not "no error occurred" — the class of assertion `live-conformance-suite.md` argues for:

- Start a turn, drop the connection, wait: the assistant node **is on disk** with the reply.
- Drop and reconnect mid-turn: the second connection receives the completed answer.
- Two concurrent attaches to one conversation both see the same terminal event.
- Cancel over the API: the turn stops *and* nothing is persisted — the current rollback guarantee,
  which must survive this change rather than being traded away for it.
- A turn interrupted by a daemon restart does not present as still running.

## What this does not fix

The subagent's report content problem (a one-sentence description instead of the comparison it
claims to have compiled) is unrelated and stays open. A durable turn will faithfully persist a thin
answer.
