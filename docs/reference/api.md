# Liberado — Interface (clients & API)

Liberado has **one client-facing API**: the HTTP + SSE surface served by `liberado-server`.
Every client — the browser web UI today, the terminal UI (TUI) later — is an HTTP client to that
server. There is no second protocol. This is the deliberate design that lets the webui and the TUI
share everything.

```
              ┌──────────── liberado-server (HTTP + SSE) ──────────────────┐
              │  /api/status   /api/models (+ POST select)   /api/catalog   │
              │  /api/reactions   /api/vault   /api/chat (+ stream)         │
              │  /api/conversations   /api/conversations/{id} (GET/PATCH)   │
              │  /api/goals  /api/goals/{id}/stream  (goal sessions)        │
              └───────────────┬───────────────────────────┬────────────────┘
                              │                            │
                    fetch / EventSource            reqwest (native)
                              │                            │
                   ┌──────────┴─────────┐        ┌─────────┴──────────┐
                   │  webui (Dioxus/WASM)│        │  tui (ratatui)     │
                   └────────────────────┘        └────────────────────┘
```

The server wraps the daemon + the conversational agent; clients are thin and stateless. Conversation
state (history) lives **server-side**, persisted by the `ConversationStore` (Decision 17) and keyed
by session id — the server rehydrates a conversation per turn from the store, so a client only sends
the next message (plus the session id) and renders the stream back.

## Goal sessions (agentic loops — surfaces as clients)

Domain-neutral goal sessions (`liberado-session`) let TUI/WebUI **start / watch / cancel** goals
without owning the loop, and — for *interactive* sessions — **answer** a session that has paused for
human input. Packs registered at boot: **`life`** (always, second-domain demo) and **`coding`**
(when a provider is configured).

| Method | Path | Body / notes |
|---|---|---|
| GET | `/api/sessions` | **Every session, newest first — chats *and* goal sessions in one list (S5′).** `goal` is absent on a chat and present on a session that runs to a terminal status; that `Option` is the only difference. Also carries `title`, `status`, `awaiting_input`, `visibility` (`foreground`/`background`), and `parent_session` (a real id, so the session tree is walkable). This is what a switcher should read. |
| GET | `/api/goals/domains` | The **registered packs** — `{ "domains": ["life","coding","dispatch"] }` when a provider is configured. `dispatch` is the dispatcher + orchestrator pair as a pack, so cron, webhooks and `delegate` are hosted sessions on the same hub as `/spawn` rather than a second engine. |
| POST | `/api/sessions/{id}/fork` | **Branch a conversation, keeping the original.** Body: `{"after_turn": <n>?, "title": <string>?}`. `after_turn` is 1-based and names the branch point by the *human's* turn — keep through turn `n` and the reply it got, dropping everything after, i.e. "go back to just after turn n and take a different path". Omit it to fork the whole conversation as it stands. The server resolves turn → node; the store speaks node ids. Returns `{id, forked_from, kept_turns, total_turns}`. **Copy, not reference**: the prefix is copied into the fork's own log, so continuing the original afterwards does not move the fork, and every session's log stays self-contained. 400 if the session has no message transcript (a goal session records *events*, not turns), 404 if unknown. |
| GET | `/api/goals` | The **kernel lens**: only goal-bearing sessions (newest first). A goal-less session has no `GoalSessionRecord` representation, so it cannot appear here. Prefer `/api/sessions` for a list; this is for callers that specifically want run-to-terminal sessions. |
| POST | `/api/goals` | JSON `GoalSpec`: `description`, `domain` (`life`\|`coding`\|`dispatch`), `success_criteria`, optional `profile`, optional `max_idle_secs`, optional `payload` |
| GET | `/api/goals/{id}` | Snapshot: record (incl. `awaiting_input`) + event history |
| GET | `/api/goals/{id}/stream` | SSE: catch-up then live `session_started`, `tool_*`, `awaiting_input`, `session_finished`, … |
| POST | `/api/goals/{id}/cancel` | Cooperative cancel |
| POST | `/api/goals/{id}/message` | JSON `{"text":"…"}` — deliver a human reply to an `awaiting_input` prompt. `202` accepted; `404` unknown session; `409` already-finished session; `403` the session's grant omits `AskHuman`, so it may **never** receive input |

SSE `data` is the full `SessionEvent` JSON (envelope `session_id`/`at` + kind fields). Event names
match kind tags (`session_started`, `tool_started`, `failed`, …) — the **same converged vocabulary
the chat stream uses** (below), so one client decoder
(`chat_client_contract::SessionEvent::from_sse_data`) serves both streams. Clients must not
reimplement tools/sandbox — only render.

**Interactive sessions.** A pack may emit `awaiting_input` (`{"prompt","options"}`) to pause for a
human answer; the record's `awaiting_input` flag flips true so a listing can badge it. The client
answers with `POST /api/goals/{id}/message`, which delivers the text into the session **and** echoes
it back into the transcript as a `human_input` (`{"text"}`) event, so the history is complete for any
later subscriber.

**A pack may ask mid-run, and the answer changes what it does.** The coding pack asks during intake
and — since E5 — again when a build fails and its grant carries `AskHuman`: it stops, asks, waits, and
folds the reply into the next attempt's feedback. The ask is **bounded** (`overrides.max_mid_run_asks`,
default 1): one ask means one guided retry, not one ask per failure. A pack that could ask on every
uncertainty would be worse than one that guesses.

**How long it waits.** `max_idle_secs` bounds the wait at a prompt before the session terminates
`budget_exhausted`. It resolves from the session profile's `max_idle_secs` (E5), with `GoalSpec`'s own
value winning when set. Interactive coding profiles typically want **hours** — the point is that you
can answer after work. Crons leave it short or unset.

**When nobody is watching, you get pinged.** If a session emits `awaiting_input` and the hub has no
live subscriber on its stream (`live_subscriber_count() == 0` — observed, not guessed), the configured
`Notifier` fires. Session open in the TUI: no ping. At work: ping.

**A parked session survives a restart, but cannot yet be answered.** If the daemon stops while a
session is awaiting input, replay restores it as `status: "parked"` with `awaiting_input` still true —
so the question it was holding for you is visible rather than silently erased (it used to be coerced to
`failed` with the flag wiped). No pack is hosting it, though, so `POST …/message` finds no live channel
and fails. Restarting the pack on an answer is E6-c in
[`one-execution-engine-plan.md`](../roadmap/one-execution-engine-plan.md). Clients should render
`parked` as "was waiting for you — start it again", **not** as an answerable prompt.

**Interactivity is a capability, not a request (S6).** Whether a session may prompt at all is decided
by its `SessionGrant`, not by the caller: `payload.interactive` is only a *request*. A session whose
grant omits `Capability::AskHuman` is handed a closed input channel — it runs to completion **without
ever prompting**, even when `interactive: true` was passed — and `POST …/message` returns `403`
(never allowed), which is deliberately distinct from `409` (allowed once, but finished).

**Session profiles (S6).** `GoalSpec.profile` names a `[[session_profiles]]` entry in
`topology.toml`, which selects three things: the **pack** that runs it (`domain`), the **capability
grant** that bounds it (`component` — a key into `policy.toml`'s `[[grants]]`, defaulting to the
profile's own name), and an **opaque `overrides`** table the pack parses itself. With no profile, a
session runs its `domain` pack under the grant keyed by the domain name (`life`, `coding`). Because
the profile picks the pack, `domain` in the request body is advisory — a profile overrides it. The
`domain` field is a plain pack-name string; an unrecognized one is accepted at the JSON boundary (it
may be a profile name) and only fails at start if no such pack is registered.

## The chat stream contract (the spine)

`POST /api/chat/stream` with `{"message": "..."}` returns `Content-Type: text/event-stream`. The body
is a sequence of SSE events; a client renders them in order.

> **Wire change, 2026-07-11 (converged session-event vocabulary):** the former chat-only names
> `tool` / `tool_result` / `done` were replaced by `tool_started` / `tool_finished` /
> `session_finished`, and `failed` became JSON `{"message"}` — one vocabulary shared with
> `/api/goals/{id}/stream`. All in-repo clients (TUI, CLI, WebUI) moved atomically; there is no
> compatibility shim for the old names.

| `event:` | `data:` | Client does |
|---|---|---|
| `session`          | the conversation id for this stream (bare, not JSON) | record it and send it back as `?session=…` (or the `session` field) on the next turn, to continue this conversation |
| `token`            | a text delta of the answer (bare, not JSON) | append to the current assistant message |
| `tool_started`     | JSON `{"name","args_preview"}` — a tool call starting (`args_preview` is a truncated preview of its arguments) | show "calling `<name>`…" with the args (legibility) |
| `tool_finished`    | JSON `{"name","ok","result_preview"}` — that call finished (`ok` = succeeded; `result_preview` is a truncated result/error) | update the chip to ✓/✗ with the preview |
| `session_finished` | JSON `{"status","summary"}` — chat turns finish with `status: "done"` | finalize the message, stop reading |
| `failed`           | JSON `{"message"}` | show it; stop. (Named `failed`, not `error`, because browser `EventSource` reserves the `error` event for its own connection errors.) |

Goal-session streams may additionally emit `session_started`, `role_started`, `role_finished`,
`progress`, `validation_finished`, and `loop_guard` — same decoder; chat clients ignore what they
don't render.

The `session` event is emitted **first**, before any agent events. A turn carries its conversation
either by the `session` field on the chat request body (`POST`) or the `?session=…` query (`GET`);
omitting it starts a new conversation, whose id the `session` event then reports back so the client
can continue it. Conversation history is rehydrated from the store each turn and persisted on success
(see *Persistence* below). The non-streaming `POST /api/chat` mirrors this: its response is
`{"reply","session"}`, so a client learns the id there too.

Two read endpoints back the conversation sidebar: **`GET /api/conversations`** lists every
conversation header (`[{id,title,created_at}]`, newest first), and **`GET /api/conversations/{id}`**
returns one conversation's full message history (`{"messages":[…]}`; `404` if it doesn't exist).
**`PATCH /api/conversations/{id}`** with `{"title": "..."}` renames a conversation (`200` on success,
`404` if it doesn't exist, `503` if chat is disabled).

> **Since S5′ these are *lenses*, not separate stores.** Chats and goal sessions live in one
> converged `Session` store, so `/api/conversations` is the **chat lens** — and it therefore lists
> *every* session, including goal sessions (whose title falls back to their goal). `/api/goals` is
> the **kernel lens** and shows only goal-bearing ones. For a list of everything as one kind of
> thing, read **`/api/sessions`**; the two older endpoints remain because a caller often legitimately
> wants exactly one of the two views.

**`GET /api/catalog`** returns the live MCP capability catalog (`{"mcps":[{name,description,
consequence,tool_count,tool_names,visible_to_main_agent,visible_to_dispatcher}]}`) — the same
`Arc<CapabilityCatalog>` the dispatcher routes against, not an independent snapshot.
`tool_count`/`tool_names` are populated from the connected chat runtime's tools, grouped by MCP
name. Visibility flags reflect `policy.toml` grants for `"main-agent"` vs `"dispatcher"`.

## Models (live catalog + hot-swap)

| Method | Path | Notes |
|---|---|---|
| GET | `/api/models` | Live catalog from the provider's OpenAI-compatible `GET /models`, plus `current`. Soft-fails: always **200** with optional `error` so clients can still show `current`. Shape: `{"models":["…"],"current"?:"…","error"?:"…"}`. |
| POST | `/api/models/select` | Body `{"model":"…"}`. Hot-swaps the active model for **subsequent** completions (same base URL / API key; only the `model` field of chat-completions requests changes). No daemon restart. Response: same `ModelsResponse` shape with updated `current`. Empty model → `400`; no provider → `503`. |

`GET /api/status` reports `model_name` from the live provider (so it tracks hot-swap, not only boot-time config). In-flight turns keep whatever model they already started with.

TUI: `/model` opens a full-screen searchable browser over `GET /api/models`; Enter calls
`POST /api/models/select`.

Structured events are **JSON-encoded** (not bare strings) so a multi-line preview can't split across
SSE `data:` lines, and so the fields stay typed. `args_preview`/`result_preview` are truncated
server-side (~200 chars) — a chip is a glance, not a log. `tool_started` and `tool_finished` always
come in pairs around one call, in order, and before the `token`s of the answer that follows the tool
use.

The endpoint is reachable two ways, same SSE either way: **`POST`** with a JSON body (native clients
like the TUI, via `reqwest`) and **`GET /api/chat/stream?message=…`** (browsers, via `EventSource`,
which can't issue a streaming `POST`). The browser closes the `EventSource` on `session_finished`/`failed` to
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
- **Events, not raw tokens** — `tool_started` and `failed` are first-class, so tool use and failures are
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
- **Tool-call visibility (contract)** — the stream emits `tool_started` (call starting, with an args preview) and
  `tool_finished` (outcome: ok + result preview) around every tool call. The backend half is done and
  tested; the web UI renders them as inline chips (visual polish still wants a human eye).
- **Stop / cancel** — closing the stream aborts the in-flight turn and rolls history back (see
  *Stop / cancel* above). Connection-lifecycle, no endpoint. The web UI's Stop button
  (`crates/webui/src/components/chat.rs`'s `stop-btn`, wired to `close_current_stream()`) calls
  `EventSource.close()` on click — both halves are landed, not just the backend primitive.
- **Sessions / persistence** — conversations are keyed by session id and persisted via the
  `ConversationStore` (Decision 17, `liberado-conversation-store`): an append-only log of message
  *nodes* (a DAG, so branching is additive), JSONL outside the vault under
  `<LIBERADO_DATA_DIR>/conversations`, so chats survive restarts. The chat requests gained a `session`
  field, the stream emits a `session` event, and `GET /api/conversations` + `GET /api/conversations/{id}`
  list and reopen them. All persistence orchestration lives in `main-agent`'s `ChatSessions` (one code
  path the web server and a future TUI-hosting daemon share), depending on the `ConversationStore`
  *trait* so the engine stays swappable. The server holds no in-memory conversation cache — it
  rehydrates per turn and persists a turn's messages only on success.

### Landed (continued — web UI polish)
- **Markdown rendering** — the agent's Markdown answers render as code blocks, lists, and links
  (`crates/webui/src/components/markdown.rs`, wired into `chat.rs`), not raw text.
- **MCP panel + sidebar** — `/api/catalog` and `/api/conversations` back a live capability panel and
  conversation sidebar in the web UI.

### Then (capability)
- **Reaction feed in chat** — surface the daemon's autonomous `Reaction`s (already on `/api/reactions`)
  as system messages in the conversation, so proactive work shows up where the user is looking.

### The TUI (proved the shared API)
Both native clients are landed and share code (`chat-client-contract`'s `ChatEvent`/SSE decoder,
`liberado-commands`' slash-command dispatcher — see
[`tui-shared-code-extraction-plan.md`](../roadmap/tui-shared-code-extraction-plan.md)):
- **`liberado chat`** — the first native `reqwest`/SSE client of the contract: a terminal REPL
  (`crates/cli`, `chat_client.rs`) that streams the *same* `POST /api/chat/stream` and prints the
  conversation. It embeds no agent logic — it learns the session id from the `session` event and
  reuses it. Run it against a running server (`LIBERADO_SERVER` overrides the default
  `http://127.0.0.1:4201`).
- **`crates/tui`** (ratatui) — a native client hitting the *same* endpoints with `reqwest`: a chat
  pane consuming `/api/chat/stream`, a status line from `/api/status`, slash commands via
  `liberado-commands`, full-screen `/session` browser and `/model` model picker (`GET`/`POST`
  models APIs). Confirms the API is genuinely client-agnostic — model hot-swap and session
  browsing are ordinary HTTP, not client-specific RPC.

### Persistence paths (ops, not vault)

| Path | Contents |
|---|---|
| `<LIBERADO_DATA_DIR>/conversations/*.jsonl` | Face chat history (Decision 17) |
| `<LIBERADO_DATA_DIR>/dispatches/chat-delegate-*.jsonl` | Mesh delegation journals (classify + disposition; linked from `delegate` tool results) |
| Platform config `liberado/settings.toml` | Client UI prefs (e.g. TUI theme name) — not daemon data |

## Design rules for keeping the API shared

1. **No client-specific endpoints.** If the web UI needs data, it goes through an endpoint the TUI
   could also call. Rendering differences (HTML vs. cells) live entirely in the client.
2. **Events over payloads.** Stream typed events (`token`/`tool_started`/`tool_finished`/`session_finished`/`failed`), not pre-rendered
   HTML — so each client renders natively.
3. **Server owns state, clients stay thin.** Conversation/session/history live server-side; a client
   is a renderer + an input box.
