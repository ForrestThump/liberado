# Liberado — Interface (clients & API)

Liberado has **one client-facing API**: the HTTP + SSE surface served by `liberado-server`.
Every client — the browser web UI today, the terminal UI (TUI) later — is an HTTP client to that
server. There is no second protocol. This is the deliberate design that lets the webui and the TUI
share everything.

```
              ┌──────────── liberado-server (HTTP + SSE) ──────────────────┐
              │  /api/status   /api/reactions   /api/vault                  │
              │  /api/chat (POST, non-streaming)                            │
              │  /api/chat/stream (POST/GET → text/event-stream)            │
              │  /api/conversations   /api/conversations/{id}               │
              └───────────────┬───────────────────────────┬────────────────┘
                              │                            │
                    fetch / EventSource            reqwest (native)
                              │                            │
                   ┌──────────┴─────────┐        ┌─────────┴──────────┐
                   │  webui (Dioxus/WASM)│        │  tui (ratatui)     │  ← future
                   └────────────────────┘        └────────────────────┘
```

The server wraps the daemon + the conversational agent; clients are thin and stateless. Conversation
state (history) lives **server-side**, persisted by the `ConversationStore` (Decision 17) and keyed
by session id — the server rehydrates a conversation per turn from the store, so a client only sends
the next message (plus the session id) and renders the stream back.

## The chat stream contract (the spine)

`POST /api/chat/stream` with `{"message": "..."}` returns `Content-Type: text/event-stream`. The body
is a sequence of SSE events; a client renders them in order:

| `event:` | `data:` | Client does |
|---|---|---|
| `session`     | the conversation id for this stream | record it and send it back as `?session=…` (or the `session` field) on the next turn, to continue this conversation |
| `token`       | a text delta of the answer | append to the current assistant message |
| `tool`        | JSON `{"name","args"}` — a tool call starting (`args` is a truncated preview of its arguments) | show "calling `<name>`…" with the args (legibility) |
| `tool_result` | JSON `{"name","ok","preview"}` — that call finished (`ok` = succeeded; `preview` is a truncated result/error) | update the chip to ✓/✗ with the preview |
| `done`        | *(empty)* | finalize the message, stop reading |
| `failed`      | an error message | show it; stop. (Named `failed`, not `error`, because browser `EventSource` reserves the `error` event for its own connection errors.) |

The `session` event is emitted **first**, before any agent events. A turn carries its conversation
either by the `session` field on the chat request body (`POST`) or the `?session=…` query (`GET`);
omitting it starts a new conversation, whose id the `session` event then reports back so the client
can continue it. Conversation history is rehydrated from the store each turn and persisted on success
(see *Persistence* below). The non-streaming `POST /api/chat` mirrors this: its response is
`{"reply","session"}`, so a client learns the id there too.

Two read endpoints back the conversation sidebar: **`GET /api/conversations`** lists every
conversation header (`[{id,title,created_at}]`, newest first), and **`GET /api/conversations/{id}`**
returns one conversation's full message history (`{"messages":[…]}`; `404` if it doesn't exist).

Tool events are **JSON-encoded** (not bare strings) so a multi-line preview can't split across SSE
`data:` lines, and so the fields stay typed. `args`/`preview` are truncated server-side (~200 chars)
— a chip is a glance, not a log. `tool` and `tool_result` always come in pairs around one call, in
order, and before the `token`s of the answer that follows the tool use.

The endpoint is reachable two ways, same SSE either way: **`POST`** with a JSON body (native clients
like the TUI, via `reqwest`) and **`GET /api/chat/stream?message=…`** (browsers, via `EventSource`,
which can't issue a streaming `POST`). The browser closes the `EventSource` on `done`/`failed` to
stop it auto-reconnecting and re-firing the turn.

### Stop / cancel

**Closing the connection cancels the turn.** There is no stop *endpoint* — a client stops by dropping
the stream (browser: `EventSource.close()`; native: drop the `reqwest` response). The server notices
the receiver is gone, **aborts the in-flight turn** (the model request and any running tool are
dropped at their next await), and **rolls the conversation history back** to before that turn — so a
stopped turn is a clean no-op: no half-written answer, and no assistant `tool_calls` left without
their results to corrupt the next turn. The conversation lock is released immediately, so the next
message starts fresh. This is one primitive that works identically for every client, which is why
it's a connection lifecycle event and not a bespoke endpoint.

Properties that make this client-agnostic:
- **Standard SSE** — consumable by browser `fetch`-streaming (WASM can't use reqwest streaming) and
  by native `reqwest` byte-streaming in the TUI. Same bytes, two parsers.
- **One POST endpoint** (not GET+query) — messages aren't length-limited or URL-encoded, and there's
  no `EventSource` auto-reconnect re-firing the turn.
- **Events, not raw tokens** — `tool` and `failed` are first-class, so tool use and failures are
  legible in any client without server changes.
- **Stateless client** — the server owns the conversation; a client reconnecting just sends the next
  message.

A non-streaming `POST /api/chat` (`{message} → {reply}`) is kept as a simple fallback for clients
that don't want to stream.

## Interface roadmap

Ordered by feel-impact. Each is a client + (sometimes) server change behind the same contract.

### Landed
- **Multi-turn chat with context + tool use** — `POST /api/chat`, server-side `Conversation`.
- **Token streaming** — `POST /api/chat/stream` (the contract above); web UI renders it live.
- **Tool-call visibility (contract)** — the stream emits `tool` (call starting, with args) and
  `tool_result` (outcome: ok + result preview) around every tool call. The backend half is done and
  tested; the web UI renders them as inline chips (visual polish still wants a human eye).
- **Stop / cancel (backend)** — closing the stream aborts the in-flight turn and rolls history back
  (see *Stop / cancel* above). Connection-lifecycle, no endpoint. The web UI still wants a literal
  **Stop button** wired to `EventSource.close()` — small frontend follow-up.
- **Sessions / persistence** — conversations are keyed by session id and persisted via the
  `ConversationStore` (Decision 17, `liberado-conversation-store`): an append-only log of message
  *nodes* (a DAG, so branching is additive), JSONL outside the vault under
  `<LIBERADO_DATA_DIR>/conversations`, so chats survive restarts. The chat requests gained a `session`
  field, the stream emits a `session` event, and `GET /api/conversations` + `GET /api/conversations/{id}`
  list and reopen them. All persistence orchestration lives in `main-agent`'s `ChatSessions` (one code
  path the web server and a future TUI-hosting daemon share), depending on the `ConversationStore`
  *trait* so the engine stays swappable. The server holds no in-memory conversation cache — it
  rehydrates per turn and persists a turn's messages only on success.

### Next (web UI polish, same contract)
- **Markdown rendering** — the agent answers in Markdown; render code blocks, lists, links instead of
  raw text.
- **Stop button (web UI)** — a button that calls `EventSource.close()` mid-stream; the backend cancel
  primitive above does the rest.

### Then (capability)
- **Reaction feed in chat** — surface the daemon's autonomous `Reaction`s (already on `/api/reactions`)
  as system messages in the conversation, so proactive work shows up where the user is looking.

### The TUI (proves the shared API)
- **`liberado chat`** (landed) — the first native `reqwest`/SSE client of the contract: a terminal
  REPL (`crates/cli`, `chat_client.rs`) that streams the *same* `POST /api/chat/stream` and prints
  the conversation. It embeds no agent logic — it learns the session id from the `session` event and
  reuses it — so it both seeds the TUI (same bytes, same incremental SSE parser) and proves the API
  is genuinely client-agnostic. Run it against a running server (`LIBERADO_SERVER` overrides the
  default `http://127.0.0.1:4201`).
- **`crates/tui`** (ratatui) — a native client hitting the *same* endpoints with `reqwest`: a chat
  pane consuming `/api/chat/stream`, a status line from `/api/status`, a reactions tail from
  `/api/reactions`. Building it is the test that the API is genuinely client-agnostic; anything the
  TUI needs that isn't expressible over HTTP/SSE is a gap in the contract, not the TUI.

## Design rules for keeping the API shared

1. **No client-specific endpoints.** If the web UI needs data, it goes through an endpoint the TUI
   could also call. Rendering differences (HTML vs. cells) live entirely in the client.
2. **Events over payloads.** Stream typed events (`token`/`tool`/`tool_result`/`done`/`failed`), not pre-rendered
   HTML — so each client renders natively.
3. **Server owns state, clients stay thin.** Conversation/session/history live server-side; a client
   is a renderer + an input box.
