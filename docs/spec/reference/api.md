# Liberado — Interface (clients & API)

Liberado has **one client-facing API**: the HTTP + SSE surface served by `liberado-server`.
Every client — the browser web UI today, the terminal UI (TUI) later — is an HTTP client to that
server. There is no second protocol. This is the deliberate design that lets the webui and the TUI
share everything.

```
              ┌──────────── liberado-server (HTTP + SSE) ──────────────────┐
              │  /api/status   /api/models (+ POST select)   /api/catalog   │
              │  /api/reactions   /api/vault   /api/library   /api/chat    │
              │  /api/conversations   /api/conversations/{id} (GET/PATCH)   │
              │  GET /api/conversations/{id}/grant                           │
              │  GET /api/conversations/{id}/grants (read-only)              │
              │  GET /api/approvals/pending                                  │
              │  /api/conversations/{id}/approvals   POST .../resolve        │
              │  /api/goals  /api/goals/{id}/stream  (goal sessions)        │
              │  GET /api/schedules   GET /api/hooks   POST /api/hooks/{name} │
              │  /api/push/status  /api/push/subscriptions  /api/push/test │
              │  POST /api/push/weekly                                     │
              └───────────────┬───────────────────────────┬────────────────┘
                              │                            │
                    fetch / EventSource            reqwest (native)
                              │                            │
                   ┌──────────┴─────────┐        ┌─────────┴──────────┐
                   │  webui (Dioxus/WASM)│        │  tui (ratatui)     │
                   └────────────────────┘        └────────────────────┘
```

The server wraps the daemon + the conversational agent; clients are thin and stateless. Every
matched `/api/*` request is logged at INFO with `method`, `path` (no query string), `status`, and
`latency_ms`. These routes log at DEBUG instead so polling and long-lived streams do not flood
INFO: `GET /api/health`, `GET /api/auth/session`, `GET /api/status` (Status view poll, 10s),
`GET /api/chat/stream`, `GET /api/conversations/{id}/attach`, `GET /api/conversations/{id}/approvals`
(approval cards poll, 3s), and `GET /api/goals/{id}/stream`. Bodies, headers, cookies, and query
strings are never logged. Push subscribe/unsubscribe/test
handlers log outcome with push service host and subscription id/count only (never endpoint paths or
keys). Conversation state (history) lives **server-side**, persisted by the `ConversationStore` (Decision 17) and keyed
by session id — the server rehydrates a conversation per turn from the store, so a client only sends
the next message (plus the session id) and renders the stream back.

## Goal sessions (agentic loops — surfaces as clients)

Domain-neutral goal sessions (`liberado-session`) let TUI/WebUI **start / watch / cancel** goals
without owning the loop, and — for *interactive* sessions — **answer** a session that has paused for
human input. Packs registered at boot: **`life`** (always, second-domain demo) and **`coding`**
(when a provider is configured **and** the daemon was built with the `coding` feature).
A default build omits `coding` from `GET /api/goals/domains`. `POST /api/goals` with
`domain` `coding`, and rewind of a coding session, return 400 and the body contains
`` `coding` ``.

| Method | Path | Body / notes |
|---|---|---|
| GET | `/api/sessions` | **Every session, newest first — chats *and* goal sessions in one list (S5′).** `goal` is absent on a chat and present on a session that runs to a terminal status; that `Option` is the only difference. Also carries `title`, `status`, `awaiting_input`, `visibility` (`foreground`/`background`), and `parent_session` (a real id, so the session tree is walkable). This is what a switcher should read. |
| GET | `/api/goals/domains` | The **registered packs** — `{ "domains": ["life","coding","dispatch"] }` when a provider is configured and the daemon was built with `coding`. Without that feature the list has no `coding` entry. `dispatch` is the dispatcher + orchestrator pair as a pack, so cron, webhooks and `delegate` are hosted sessions on the same hub as `/spawn` rather than a second engine. |
| GET | `/api/schedules` | **Read-only** reflection of `[[schedules]]` from loaded topology: `{ "schedules": [{ "name", "enabled", "cron_expr", "days", "at", "goal", "job", "category", "profile", "next_fire": null, "description", "schedule_text", "timezone" }] }`. A structured row (`days` + `at`) is evaluated in its `timezone`, or in `topology.timezone` when the row omits one. `schedule_text` for that row is civil English such as `Every Tuesday at 8:00 PM America/Chicago`. A `cron_expr` row is UTC unless it sets `timezone`, in which case its fields are civil time in that zone. `next_fire` is the next fire in the zone used for that row, for example `Tue Nov 3, 8:00 PM CST`. It is null when the row does not compile. `description` is plain text for what that row runs, built from its kind, goal, profile, and delivery fields. `category` is `system` for jobs `vault-reminder-tick` and `weekly-spending-push`, and `automation` for every other row. For a UTC `cron_expr`, `schedule_text` converts the clock into `topology.timezone` using the offset at request time. An expression the reader cannot describe exactly is returned unchanged as `schedule_text`. No enable/disable, run-now, or topology write. |
| GET | `/api/hooks` | **Read-only** list of configured webhook hooks: `{ "hooks": [{ "name", "enabled" }] }`. Never returns `secret_ref` or secret values. Triggers remain `POST /api/hooks/{name}` with `X-Liberado-Hook-Secret`. |
| GET | `/api/push/status` | `{ "vapid_configured", "reminder_enabled", "subscription_count", "public_key" }`. `public_key` is null when Web Push keys are missing. No private key. |
| GET | `/api/push/vapid-public-key` | `{ "public_key" }` or 503 `{ "error": "Web Push keys are not configured" }`. |
| POST | `/api/push/subscriptions` | Body `{ "endpoint", "keys": { "p256dh", "auth" }, "label" }`. Upsert by endpoint. 400 when the endpoint or keys are unusable. |
| DELETE | `/api/push/subscriptions` | Body `{ "endpoint" }`. Removes that phone. |
| POST | `/api/push/test` | Optional `{ "endpoint" }` or `{ "id" }`. Enqueues one test row for that phone. 503 when keys are missing, 409 when no phone is subscribed, 404 when the named phone is absent. Does not change `last_sent_week`. |
| POST | `/api/push/weekly` | Body `{ "dry_run": false, "force": false }`. Returns `{ "message", "week_id", "start", "end", "unmapped", "sent", "failed", "skipped", "removed" }`. Each `unmapped` entry is a category and signed cents, for example `Shipping $5.55`. The message names each bucket as spent of budget, then `$X left` or `over by $X`. `dry_run` wins over `force` and works while the reminder is off. A real send while the reminder is off is 409. A missing budget URL or key is 503 `budget API is not configured`. |
| POST | `/api/sessions/{id}/fork` | **Branch a conversation, keeping the original.** Body: `{"after_turn": <n>?, "title": <string>?}`. `after_turn` is 1-based and names the branch point by the *human's* turn — keep through turn `n` and the reply it got, dropping everything after, i.e. "go back to just after turn n and take a different path". Omit it to fork the whole conversation as it stands. The server resolves turn → node; the store speaks node ids. Returns `{id, forked_from, kept_turns, total_turns}`. **Copy, not reference**: the prefix is copied into the fork's own log, so continuing the original afterwards does not move the fork, and every session's log stays self-contained. 400 if the session has no message transcript (a goal session records *events*, not turns), 404 if unknown. |
| GET | `/api/goals` | The **kernel lens**: only goal-bearing sessions (newest first). A goal-less session has no `GoalSessionRecord` representation, so it cannot appear here. Prefer `/api/sessions` for a list; this is for callers that specifically want run-to-terminal sessions. |
| POST | `/api/goals` | JSON `GoalSpec`: `description`, `domain` (`life`\|`coding`\|`dispatch`), `success_criteria`, optional `profile`, optional `max_idle_secs`, optional `payload`. Domain `coding` is accepted only when the daemon was built with the `coding` feature. Otherwise the response is 400 and the body contains `` `coding` ``. |
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
[`one-execution-engine-plan.md`](../../future-work/archive/one-execution-engine-plan.md). Clients should render
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

A profile may also pin `model` (the slug sent on each turn) and `provider` (a `[[providers]]` name,
for example `openrouter`). The slug is then sent to **that** backend. Omit `provider` and the slug
goes to `topology.provider`, which is the daemon default. An unknown provider id, or a provider
entry whose `base_url` or `api_key_env` is empty, fails config load. An unset API-key environment
variable does not fail load — a missing key on the default provider already boots the daemon
watch-only — but a turn on that profile fails and names the missing variable. It does not send the
slug to the default backend. The provider is read from the profile on every turn, so changing it
applies to sessions that already pinned a model. The choice is not stored on the session grant.

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
`{"reply","session"}`, so a client learns the id there too. It opens the conversation with the
same rules as the stream. `incognito`, `background`, and `profile` apply only when `session` is
absent. `incognito` wins over `background`. A resolved `profile` is the session grant. An unknown
or disabled `profile` creates nothing: this endpoint returns HTTP 400 and `{"error"}`, and the
stream puts that same text on a `failed` event. A `profile` sent with an existing `session` does
not retarget that conversation; an unknown name still fails the request.

Two read endpoints back the conversation sidebar: **`GET /api/conversations`** lists every
conversation header (`[{id,title,created_at,project?,pending_model?}]`, newest first), and
**`GET /api/conversations/{id}`** returns one conversation's full message history
(`{"messages":[…],project?,pending_model?}`; `404` if it doesn't exist).
**`PATCH /api/conversations/{id}`** updates a conversation (`200` on success, `404` if it doesn't
exist, `400` for an unknown project or an empty body, `503` if chat is disabled). The body may
include any of `title`, `project`, and `pending_model`. A missing field is left unchanged. `null`
clears `project` or `pending_model`. `project` must be an enabled name from `GET /api/projects`.
`pending_model` is the model id for the next turn. That turn stamps the same string on the message
and clears the field. It does not substitute a different model. `{"title":"..."}` still renames.

**`GET /api/conversations/{id}/grants`** is read-only. It returns the effective tool, MCP, and zone
grants for that conversation's agent and session. `agent` is the process `main-agent` grant.
`session` is the named profile's stored grant, and it is empty when no profile is set — an unnamed
header is not a grant of nothing. `effective` is `session` when a profile is named, otherwise
`agent`. `callable` lists connected tools that the effective grant permits. `404` if the
conversation does not exist, `503` if chat is disabled. There is no revoke on this route.

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
name. `visible_to_dispatcher` is the `policy.toml` grant for `"dispatcher"`.
`visible_to_main_agent` is the `"main-agent"` grant, narrowed by the chat-access ledger below.
The ledger cannot grant a server policy did not grant.

**`PUT /api/plugins/{id}`** body `{"enabled": true|false}`. `{id}` is an MCP server name from
`GET /api/catalog`. `enabled: false` hides that server from the chat face (catalog flag and the
chat runtime). `enabled: true` restores it only when policy already grants it to `main-agent`;
otherwise `409`. Unknown id → `404`. Response:
`{"name","enabled","visible_to_main_agent"}`. The mask is process-wide for the chat face, stored
in `{data_dir}/plugin-chat-access.json`. It does not edit `topology.toml` and does not change the
dispatcher grant.

**`GET /api/profiles`** lists enabled `[[session_profiles]]` under their live names.
`agent_eligible` follows the name in `topology.toml` (what `[chat] agent_profiles` lists), not a
renamed display name.

**`PATCH /api/profiles/{name}`** body `{"name":"new-name"}`. Renames one enabled profile.
`200` `{"name","renamed_to","conversations"}`. The same name is a no-op (`conversations: 0`).
Names are trimmed, non-empty, at most 64 characters, and ASCII letters, digits, `.`, `_`, `-`.
A bad name is `400`. A name already in use is `409`. An unknown name is `404`. `chat-default`
and the Agent Creator profile (`operator`) are `403`, whether they are the current name or the
requested name. Conversations that used the old name are kept; their stored grant is copied onto
the new name. The ledger is `{data_dir}/session-profile-ledger.json`. `topology.toml` is not
rewritten.

**`DELETE /api/profiles/{name}`** removes that live name. `200`
`{"name","deleted":true,"conversations_kept","fallback":"default"}`. Conversations are kept.
Their profile is cleared, so the next turn uses the process default grant. Unknown → `404`.
`chat-default` and `operator` → `403`.

## Models (live catalog + hot-swap)

| Method | Path | Notes |
|---|---|---|
| GET | `/api/models` | Live catalog from the provider's OpenAI-compatible `GET /models`, plus `current`. Soft-fails: always **200** with optional `error` so clients can still show `current`. Shape: `{"models":["…"],"current"?:"…","error"?:"…"}`. |
| POST | `/api/models/select` | Body `{"model":"…"}` hot-swaps the daemon-wide model. Body `{"model":"…","conversation":"<ulid>"}` records an in-memory pick for that chat's next turn. The durable per-chat pick is `pending_model` on `PATCH /api/conversations/{id}`. The turn uses the in-memory pick when one is set, otherwise the stored `pending_model`, and stamps that exact string. Empty model → `400`; no provider on the daemon-wide form → `503`. |

`GET /api/status` reports `model_name` from the live provider (so it tracks hot-swap, not only boot-time config). In-flight turns keep whatever model they already started with. The same payload includes `mcp`: a list of `{name, state, attempts, tools}` plus optional `last_error` and `next_retry_ms`. `state` is `connected` or `degraded`. The list is empty until the chat supervisor has started. `GET /api/health` stays `{"ok": true}` with HTTP 200 while a peer is down; the Docker healthcheck only requires that 200.

TUI: `/model` opens a full-screen searchable browser over `GET /api/models`; Enter calls
`POST /api/models/select`.

## Projects (read-only)

`GET /api/projects` returns the enabled `[[projects]]` rows from `topology.toml`:
`{ "projects": [{ "name", "root", "write_class" }] }`. There is no create, update, or delete
route. A project is declared in that file. The WebUI lists the rows and can remember a choice
in the browser. It does not write the topology.

## Library (vault files, v1)

`GET /api/library?limit=50` returns `items`. One row looks like this:

```json
{
  "id": "attachments/a.png",
  "kind": "png",
  "title": "a.png",
  "path_or_url": "attachments/a.png",
  "created_at": "2026-10-15T12:00:00Z",
  "source": "vault"
}
```

v1 is a read-only capped scan of vault folders the default profile may read. The default
profile is the enabled `chat-default` session profile. When that profile is missing or
disabled, the scan uses the `main-agent` grant. A folder is included only when that grant
has `Read` on a **vault** zone and the zone name is one of `attachments`, `media`, `images`,
`files`, `exports`, or `artifacts` (ASCII case-insensitive). Other readable zones, such as
`tasks` or `decisions`, are not library items.

`source` is always `"vault"`. Session artifacts and executor offload are not listed. There is
no upload and no delete.

`limit` defaults to 50 and is clamped to 1..=200. `created_at` is the file's last modification
time in UTC (`YYYY-MM-DDTHH:MM:SSZ`). Birth time is not portable, so v1 does not use it. The
scan stops after 2000 files and 8 directory levels under each folder, then keeps the newest
`limit` rows. A missing vault or an empty folder is `200` with `{ "items": [] }`.

`id` and `path_or_url` are the vault-relative path with `/` separators. `kind` is the
lowercased extension, or `"file"` when the name has none. `title` is the file name.

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

**Cancel cascades to delegated work.** A turn blocked in `delegate` is waiting on a hub session.
Aborting the turn alone left that child running with its own grant and nobody watching it, so a
cancel now also cancels every non-terminal hub session whose `origin.conversation_id` is this
conversation. The child ends `cancelled`, the same cooperative stop as
`POST /api/goals/{id}/cancel`. Workers started by other conversations are not touched.

Properties that make this client-agnostic:
- **Standard SSE** — consumable by browser `fetch`-streaming (WASM can't use reqwest streaming) and
  by native `reqwest` byte-streaming in the TUI. Same bytes, two parsers.
- **One POST endpoint** (not GET+query) — messages aren't length-limited or URL-encoded, and there's
  no `EventSource` auto-reconnect re-firing the turn.
- **Events, not raw tokens** — `tool_started` and `failed` are first-class, so tool use and failures are
  legible in any client without server changes.
- **Stateless client** — the server owns the conversation; a client reconnecting just sends the next
  message.

A non-streaming `POST /api/chat` (`{message} → {reply, session}`) is kept as a simple fallback for
clients that don't want to stream. It uses the same session-opening rules as the stream, including
`profile`, `incognito`, and `background`.

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
[`tui-shared-code-extraction-plan.md`](../../future-work/archive/tui-shared-code-extraction-plan.md)):
- **`liberado chat`** — the first native `reqwest`/SSE client of the contract: a terminal REPL
  (`crates/cli`, `chat_client.rs`) that streams the *same* `POST /api/chat/stream` and prints the
  conversation. It embeds no agent logic — it learns the session id from the `session` event and
  reuses it. Run it against a running server (`LIBERADO_SERVER` overrides the default
  `http://127.0.0.1:4201`). When `[api]` credentials are configured, set
  `LIBERADO_API_TOKEN` to a `[[api.clients]]` token.
- **`crates/tui`** (ratatui) — a native client hitting the *same* endpoints with `reqwest`: a chat
  pane consuming `/api/chat/stream`, a status line from `/api/status`, slash commands via
  `liberado-commands`, full-screen `/session` browser and `/model` model picker (`GET`/`POST`
  models APIs). Confirms the API is genuinely client-agnostic — model hot-swap and session
  browsing are ordinary HTTP, not client-specific RPC.

acp-bridge face mode and `liberado shepherd` are native daemon clients of the same API. When
`[api]` credentials are configured, set `LIBERADO_API_TOKEN` to a `[[api.clients]]` token for
each of them. Shepherd sends that bearer only on its goal calls. GitHub access stays on `gh`.

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

## Trust boundary: credentials, sessions, and passkeys

The HTTP API is **not** open on the tailnet by default: it binds loopback unless you configure
`[api]` and widen `bind`. Every route under `/api` (except the small unauthenticated band documented
above) requires either:

- **`Authorization: Bearer`** with the owner human token or a `[[api.clients]]` token, or
- A **browser session** minted by `POST /api/auth/login` (emergency token) or passkey sign-in,
  carried as `HttpOnly` cookies plus `X-Liberado-CSRF` on mutating requests.

Owner-only routes (approvals resolve, profile changes, push admin, …) require the **human**
principal — client tokens authenticate but cannot decide.

When `[api.webauthn]` is configured, the WebUI can register and sign in with passkeys on the listed
https origins (or `http://localhost` for dev). Sessions are stored hashed in `liberado.db` with a
30-day absolute lifetime and 14-day idle timeout. The human token remains available for scripts
(`Authorization: Bearer`) and emergency browser sign-in.

Cross-origin browser access still requires explicit `allowed_origins`; there is no wildcard CORS.
Agent tools that can `GET` URLs may still reach loopback if granted a web-fetch MCP — treat MCP
grants as part of the trust boundary.

Mutating routes use `POST`/`PATCH`/`DELETE` so a `GET`-only fetcher cannot decide approvals or
change profiles. A web-fetching MCP granted to an agent can still read anything exposed via `GET`
on loopback if it can reach the bind address — scope MCP grants accordingly.

## Reminders and Web Push (owner + device)

| Endpoint | Auth | Role |
|---|---|---|
| `GET /api/push/status` | owner | VAPID configured, `reminder_enabled` (`[weekly_spending].enabled`), subscription count, public key |
| `GET /api/push/vapid-public-key` | owner | Application server key for `pushManager.subscribe` |
| `GET/POST/DELETE /api/push/subscriptions` | owner | List, upsert, remove device subscriptions |
| `POST /api/push/test` | owner | Enqueue a test Web Push row for one subscription (`endpoint` or `id` in the JSON body; must match a registered device). Does not change `last_sent_week`. |
| `POST /api/push/weekly` | owner | Body `{ "dry_run": false, "force": false }`. Returns `{ "message", "week_id", "start", "end", "unmapped", "sent", "failed", "skipped", "removed" }`. `dry_run` wins over `force` and works while the reminder is off. A real send while the reminder is off is 409. A missing budget URL or key is 503 `budget API is not configured`. A successful send stores that Monday on the subscription. A failed send does not. |
| `POST /api/client-errors` | owner | Truncated WebUI panic/client error text at WARN (no secrets) |
| `GET /api/outbox` | owner | Visible pending/failed reminder rows |
| `GET /api/completions` | owner | Task-completion log. Query: `since` (RFC3339, optional) and `limit` (optional, default 100, maximum 500). Rows with `completed_at` at or after `since`, oldest first. Each row has `id`, `completed_at`, `local_date`, `precision` (`tap`, `tick`, or `date`), `source` (`liberado` or `obsidian`), `path`, `task_text`, `done_date`, `outbox_id`, `voided_at`, `detected_at`. Voided rows stay in the list with `voided_at` set. No credential: 401. A client token: 403. |
| `GET /api/outbox/{id}` | owner | One outbox row for the notification detail page (`title`, `body`, `kind`, `due_at`, `late`, `source`, delivery/ack state, `resolution`, `resolved_at`, optional `snooze_until_local`, optional `obsidian_url`) |
| `POST /api/outbox/{id}/retry` | owner | Retry a failed row |
| `POST /api/outbox/{id}/complete` | owner | Mark a vault task done (`task_due` + `vault_note` source) or dismiss other kinds; sets `resolution` (`completed` or `dismissed`) when not already resolved (409 if resolved). A one-off line is checked and stamped ` ✅ YYYY-MM-DD` (operator local date, after other task fields, before a trailing `^block-id`). A 🔁 line is not checked and is not stamped. |
| `POST /api/outbox/{id}/snooze` | owner | `{minutes}` — atomically resolve the row as `snoozed`, enqueue one follow-up due at now+minutes (not delivered until that due time), idempotency key `snooze/{id}` |
| `POST /api/outbox/ack` | none | `{id, token}` from the push payload; sets `acked_at` only |
| `POST /api/outbox/action` | none | `{id, token, action, minutes?}` — `action` is `done` or `snooze`. The per-row push token must match that row. `done` completes the source vault task. `snooze` uses `minutes`, or 15 when `minutes` is omitted. Same row transitions as the owner complete and snooze routes. Wrong or unknown token: 403, no change. Already resolved: 409. Unknown action, bad minutes, or a row that is not a completable vault task: 400, no change. Success JSON is `{notice}` |

`POST /api/outbox/ack` and `POST /api/outbox/action` take no owner credential. The service worker
has no owner cookie and no CSRF token, and it must keep working after sign-out. The push token
is the capability. It is compared in constant time to that row's stored hash and authorizes only
that row: ack sets `acked_at`, and action runs the owner complete or snooze logic for a `task_due`
row whose source is a vault note. A token does not grant any other route. Both posts use the same
default JSON body limit as the rest of the API. Ack has no rate limiter, and action does not add
one. Subscription management and the owner detail, complete, and snooze routes stay owner-only.

Web Push JSON is version `v: 2`. The service worker reads `title`, `body`, `tag`, `rows`, and
`url` (single rows → `/notifications/<id>`, digest → `/notifications`). A body tap
(`notificationclick` with an empty `action`) acks every `{id, token}` in `rows` via
`POST /api/outbox/ack`, then focuses an open window or opens `url`.

`actions` and `snooze_minutes` are present only for one completable vault task (`kind` is
`task_due`, `source.type` is `vault_note`, and `rows` has one entry). The buttons are Done and
Snooze 15m. The 15 minutes are `DEFAULT_SNOOZE_MINUTES` in `crates/notify/src/web_push/send.rs`.
The detail page's short snooze uses that same length. Digests, tests, other kinds, and approvals
omit `actions`. Older workers ignore the extra fields. A platform that shows no action buttons
still opens the detail page on a body tap. The worker does not read `Notification.maxActions`,
so the notification is still shown.

Done posts `{action:"done"}` and does not open a window. On success the worker shows a
confirmation with the same tag and no actions (`notice`, for example `Done: <title>`). Snooze
posts `{action:"snooze", minutes: snooze_minutes}`. The confirmation is `Snoozed until HH:MM`
in the operator zone. Any failure (network, or a status that is not 2xx) acks the rows and opens
`url`, so the owner can finish on the detail page. A row completed or snoozed from the tray is
the same resolved row `GET /api/outbox/{id}` returns.

Additional fields include `kind`, `outbox_id`, `due_at` (RFC 3339 UTC), `late`, `source`, and
digest `items`. Outbox rows store nullable `kind`, `source_json`, `resolution`, and `resolved_at`.
Schema v4 adds `push_subscriptions.last_sent_week`. Schema v5 adds `task_completions`,
`task_snapshot`, and `task_file_state` (a content hash so an unchanged note is skipped)
in the same `liberado.db`. A database already stamped v4 keeps `last_sent_week` and gains
the completion tables. Completion
rows are not pruned. `precision` is `tap` (Done time, to the second), `tick` (the minute the
file change was seen, plus sync delay), or `date` (local noon of a ✅ date that is earlier
than the detection date). `GET /api/completions` is the read API. Done on a one-off line
checks the box and appends ` ✅ YYYY-MM-DD` for the operator's local date, after the other
task fields and before a trailing `^block-id`. A line that already has ✅ is not stamped
again. The next tick attributes that checkoff to the tap and can store the written date on
that row. A line containing 🔁 is not checked and gains no ✅; the tap row is written at
the press and the vault line stays open.

`topology.obsidian_vault_name` (optional) overrides the vault name embedded in server-built
`obsidian://open` links; when unset, the basename of `topology.vault_path` is used.
