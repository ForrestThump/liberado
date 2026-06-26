# liberado-tui — Roadmap

Ordered by impact-to-effort ratio. Each feature is decomposed into a **reusable layer**
(`app.rs` state + `api.rs` types) that other UIs can import without TUI dependencies,
and a **TUI-specific layer** (`ui.rs` rendering + `main.rs` wiring).

---

## 1. Esc to cancel in-flight streaming

**Problem:** An SSE turn runs until completion or connection error. The user has no way
to stop a long-running tool call or a slow model reply without killing the whole process
(Ctrl+C). The backend already supports cancel-on-disconnect (closing the SSE stream
aborts the turn and rolls history back) — the TUI just needs to trigger it.

### Reusable layer (`app.rs`)

- **New `Effect::CancelStream`** — signals `main.rs` to abort the spawned SSE task.
- **`App::cancel_stream()`** — sets `streaming = false`, clears `assistant_buf`,
  appends a `Message::System("[cancelled]")`, returns `vec![Effect::CancelStream]`.
- **Key routing:** In `handle_input_key`, when `KeyCode::Esc` and `self.streaming`,
  call `self.cancel_stream()` instead of clearing input. The first Esc during streaming
  cancels; a second Esc on an empty non-streaming input clears the composer.

### TUI-specific layer (`main.rs`)

- **AbortHandle tracking.** When `execute_effect` spawns the SSE task for
  `StartChatStream`, store the `JoinHandle` in a new field on a shared `StreamState`
  struct (`Arc<Mutex<Option<AbortHandle>>>`).
- **`Effect::CancelStream`** handler — calls `handle.abort()` on the stored handle,
  drops the handle. The SSE task exits at its next `.await` (reqwest read), and the
  spawned task's future is dropped.
- **Stream guard.** When the SSE task finishes naturally (Done/Failed/error), clear
  the stored handle so a stale abort doesn't double-fire.

### Test plan (in `app::tests`)

- Esc during streaming returns `Effect::CancelStream` and stops `streaming`.
- Esc during streaming still clears input if already streaming=false and input is empty.
- Esc during streaming does NOT clear non-empty input (first Esc cancels, second clears).

---

## 2. Loading indicators and timestamps (DONE)

## 3. Basic markdown rendering (DONE — `liberado-markdown` crate)

## 4. Status bar / header line (DONE)

## 5. Conversation search/filter (DONE)

## 6. Theming (DONE — `liberado-theme` crate)

## 7. Conversation title polish (DONE)

## 8. Future candidates (ALL DONE)

### 8a. Conversation DAG / branching views (DONE)
Tree rendering in sidebar with collapse/expand (Enter, Space). `/fork` slash command
stub (server support pending). `conv_header.parent_conversation` drives the tree.

### 8b. User theme files (DONE)
`ThemeRegistry::load_user_themes()` and `reload()` in `liberado-theme`.
`/theme reload` hot-loads from `~/.config/liberado/themes/`.

### 8c. Stop button (DONE)
`Ctrl+S` finalizes partial response as `[stopped]`, returns `CancelStream`.
`Esc` still uses `[cancelled]`.

### 8d. Mouse support (DONE)
Click-to-focus on all three panes (input, sidebar, chat). Scroll wheel in chat and
sidebar. Double-click sidebar leaf loads conversation.

---

## 9. Production hardening (from consultant audit, 2026-06-25) — ALL DONE 2026-06-25

Issues ranked by severity from the professional audit of `crates/tui/`.

### 9.1 Fix mutex poisoning surface (CRITICAL) — DONE
Switched `std::sync::Mutex` → `parking_lot::Mutex` (ignores poisoning). Removed `.unwrap()` from all lock calls.

### 9.2 Add SSE stream timeout (HIGH) — DONE
Wrapped `stream.next()` with `tokio::time::timeout(60s)`. `SSE_STREAM_TIMEOUT` in `tuning.rs`.

### 9.3 Reset `chat_cursor` on message clear (MEDIUM) — DONE
`chat_cursor = 0` + `expanded_messages.clear()` in `cmd_clear`, `cmd_new`, `HistoryLoaded`, sidebar `n`.

### 9.4 Add EffectRunner integration tests (HIGH) — DONE
7 integration tests using `wiremock` mock server. Covers refresh, history load, cancel, fork, quit.

### 9.5 Add loading indicator during history fetch (MEDIUM) — DONE
Chat pane shows `"Loading conversation <spinner>"` when `pending_load` set and messages empty.

### 9.6 Clear `assistant_buf` on `SseFailed` (MEDIUM) — DONE
`self.assistant_buf.clear()` added to `SseFailed` handler.

### 9.7 Implement message cap with sliding window (MEDIUM) — DONE
`MAX_MESSAGE_COUNT = 500`. History loads over 500 truncate with `"N messages omitted"` marker.

### 9.8 Add graceful SIGTERM handler (MEDIUM) — DONE
`ctrlc = "3.4"`. Sets `should_quit = true` on OS signal for clean terminal restore.

### 9.9 Split `render/sidebar.rs` into 3 files (LOW) — DONE
`sidebar_status.rs`, `sidebar_reactions.rs`, `sidebar_conversations.rs`.

## 10. Fresh consultant re-audit fixes (2026-06-25) — ALL DONE

### 10.1 Bounded mpsc channel with backpressure (MEDIUM) — DONE
`unbounded_channel()` → `channel(256)`. All sends use `try_send()` with `tracing::warn!` on full.

### 10.2 Visible SSE parse errors (MEDIUM) — DONE
`SseEvent::to_action()` returns `Result<Action, String>`. Parse failures → `Action::SseFailed` visible in chat.

### 10.3 Robust mouse hit-testing (LOW) — DONE
`point_in_rect(col, row, rect)` with explicit bounds checks replaces `Rect::intersects(pt)` 1×1 rect hack.

---

## 11. Decouple into `agent-tui-core` library (PLANNED — 2026-06-25)

See `DECOMPOSITION.md` for the full analysis.

**Goal:** Split `liberado-tui` into a general-purpose agent TUI library (`agent-tui-core`)
reusable across different backends (Liberado, agentic coding platforms, chatbots), with
Liberado-specific code in a separate `agent-tui-liberado` crate.

**Feasibility:** HIGH. The architecture already separates state machine, handlers, rendering,
and effects — the coupling is only at 15 specific points where Liberado API types and endpoints
are hardcoded. 10 modules require zero changes.

### Proposed crate split

```
agent-tui-core/          (generic: App<B: Backend>, handlers, render, SSE, formatting)
agent-tui-liberado/      (Liberado-specific: providers, api types, commands, panels)
liberado-theme/          (already decoupled — no changes)
liberado-markdown/       (already decoupled — no changes)
```

### Key milestones

* **Phase 1 (3-4d): Trait extraction + GenericApp** — define `Backend` trait, make `App<B: Backend>` generic, create `Custom(B::Action)`/`Custom(B::Effect)` variants.
* **Phase 2 (2-3d): Provider traits + effect decoupling** — `ChatProvider`, `ConversationProvider`, `StatusProvider`, `ReactionProvider` traits; implement Liberado backend; refactor `spawn_poller()` and `EffectRunner`.
* **Phase 3 (2-3d): Render decoupling + command registry** — pluggable sidebar panels, configurable layout, `CommandRegistry` trait, configurable status bar.
* **Phase 4 (2d): Cleanup, docs, example backends** — rename crate, mock/echo/OpenAI example backends, documentation.

### What stays as-is (10 modules, zero changes)

`sse.rs`, `terminal.rs`, `format.rs`, `tuning.rs` (mostly), `render/input.rs`,
`render/chat.rs` (mostly), `handlers/input.rs`, `handlers/chat.rs`, `handlers/mouse.rs`,
`conversations.rs`, word-boundary utils.

### Trait surface

```rust
trait Backend { type Action; type Effect; type ConvSummary; type StatusData; }
trait ChatProvider { /* post_chat_stream → SSE stream */ }
trait ConversationProvider { /* list + get_history */ }
trait StatusProvider { /* fetch_status */ }
trait ReactionProvider { /* fetch_reactions (optional) */ }
trait CommandHandler { /* registerable slash commands */ }
```

### Estimated effort: 7-10 person-days
