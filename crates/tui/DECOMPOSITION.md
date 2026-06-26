# DECOMPOSITION.md — Liberado TUI → agent-tui-core Library

Analysis date: 2026-06-25. See `ROADMAP.md` section 11 for the implementation plan.

---

## Goal

Split `liberado-tui` into a **general-purpose agent TUI library** (`agent-tui-core`) reusable
across different backends — Liberado, agentic coding platforms, chatbots, REPLs — with
Liberado-specific code extracted into a separate `agent-tui-liberado` crate.

---

## Feasibility: HIGH

The architecture already cleanly separates concerns: `handle_key() → Vec<Effect>` /
`update() → Vec<Effect>` with unidirectional data flow. The coupling is only at the
**edges** where Liberado API types and endpoints are hardcoded. The core — state machine,
handlers, SSE parsing, rendering, formatting, terminal lifecycle — is already generic.

---

## Liberado-specific coupling points

### API types (`api.rs`)

| Component | What's coupled | Severity | Fix |
|-----------|---------------|----------|-----|
| `DaemonStatus` | `vault_path`, `watcher_active`, `dispatcher_attached`, `orchestrator_attached`, `reactions_seen` | HIGH | Extract via `StatusProvider` trait |
| `ReactionEvent` | `event_type`, `source`, `correlation_id`, `path`, `outcome` | HIGH | Extract via `ReactionProvider` trait (optional) |
| `ConvHeader` | `parent_conversation`, `spawned_by` — generic base: `id`, `title`, `created_at` | MEDIUM | Generic `ConvSummary` + optional extensions |
| `ChatMessage` | `role`, `content`, `tool_calls: Value`, `tool_call_id` — fairly generic | LOW | Keep as-is; `tool_calls` is already Value |
| `ToolCallChip` | `name`, `args` — display types, generic | LOW | Keep |
| `ToolResultChip` | `name`, `ok`, `preview` — display types, generic | LOW | Keep |

### HTTP functions (`api.rs`)

| Component | What's coupled | Severity | Fix |
|-----------|---------------|----------|-----|
| `fetch_status()` | `/api/status` endpoint | HIGH | `StatusProvider::fetch_status()` trait method |
| `fetch_reactions()` | `/api/reactions` endpoint | HIGH | `ReactionProvider::fetch_reactions()` trait method |
| `fetch_conversations()` | `/api/conversations` endpoint | MEDIUM | `ConversationProvider::list_conversations()` |
| `fetch_conversation_history()` | `/api/conversations/{id}` endpoint | MEDIUM | `ConversationProvider::get_conversation()` |
| `post_chat_stream()` | `POST /api/chat/stream` endpoint | HIGH | `ChatProvider::post_chat_stream()` trait method |

### Effect variants (`app.rs`)

| Variant | Coupling | Severity | Fix |
|---------|----------|----------|-----|
| `StartChatStream` | Sends to Liberado endpoint | HIGH | Delegate to `ChatProvider` |
| `RefreshConversations` | Calls Liberado endpoint | MEDIUM | Delegate to `ConversationProvider` |
| `LoadConversationHistory` | Calls Liberado endpoint | MEDIUM | Delegate to `ConversationProvider` |
| `ForkConversation` | Liberado DAG fork (server pending) | HIGH | Remove or make optional extension |
| `CancelStream` | Generic | NONE | Keep |
| `SetWindowTitle` | Generic | NONE | Keep |
| `Quit` | Generic | NONE | Keep |
| `None` | Generic | NONE | Keep |

### Action variants (`app.rs`)

| Variant | Coupling | Severity | Fix |
|---------|----------|----------|-----|
| `StatusUpdate(DaemonStatus)` | Liberado daemon health | HIGH | `StatusUpdate(B::StatusData)` generic |
| `ReactionsUpdate(Vec<ReactionEvent>)` | Liberado reactions | HIGH | Remove or `Custom(B::Action)` |
| `ConversationsUpdate`, `HistoryLoaded`, `SseSession`, `SseToken`, `SseTool`, `SseToolResult`, `SseDone`, `SseFailed`, `ConnectionStatus`, `Tick` | Generic concepts | LOW-NONE | Keep as-is |

### SSE event mapping (`sse.rs`)

| Component | Coupling | Severity | Fix |
|-----------|----------|----------|-----|
| `SseEvent` struct | Pure SSE — no Liberado refs | NONE | Keep |
| `SseDecoder` | Pure SSE parser | NONE | Keep |
| `to_action()` → 6 event types | `session`, `token`, `tool`, `tool_result`, `done`, `failed` — Liberado's contract | MEDIUM | Trait `Into<Action>` or `ActionBuilder` |
| Unknown event fallback | Defaults to `SseToken("")` | LOW | Configurable fallback |

### Sidebar render modules

| Module | Coupling | Severity | Fix |
|--------|----------|----------|-----|
| `sidebar_status.rs` | Renders `DaemonStatus` fields | HIGH | Pluggable `StatusWidget<T>` trait |
| `sidebar_reactions.rs` | Renders `ReactionEvent` list | HIGH | Optional pluggable `ReactionWidget<T>` |
| `sidebar_conversations.rs` | Renders conversation tree — generic | LOW | Keep |
| `render/mod.rs` layout | Status+reactions+conversations split | MEDIUM | Configurable sidebar layout |

### App state fields (`app.rs`)

| Field | Coupling | Severity | Fix |
|-------|----------|----------|-----|
| `server: String` | Liberado server URL | MEDIUM | Generic `backend_url` or provider config |
| `status: Option<DaemonStatus>` | Liberado type | HIGH | `Option<B::StatusData>` |
| `reactions: Vec<ReactionEvent>` | Liberado type | HIGH | Remove or optional feature |
| `conversations: Vec<ConvHeader>` | Generic concept | LOW | `Vec<B::ConvSummary>` |
| All other fields | Generic | NONE | Keep |

### Commands (`commands.rs`)

| Command | Coupling | Severity | Fix |
|---------|----------|----------|-----|
| `/status` | Uses `DaemonStatus` fields | HIGH | Via `StatusProvider` |
| `/fork` | Liberado DAG concept | HIGH | Remove or make optional |
| `/model` | Uses `DaemonStatus.model_name` | LOW | Via `StatusProvider` |
| `/session`, `/theme`, `/help`, `/new`, `/clear`, `/quit` | Generic | NONE-LOW | Keep; `/help` needs dynamic command list |

### Tuning constants (`tuning.rs`)

| Constant | Coupling | Severity | Fix |
|----------|----------|----------|-----|
| `REACTIONS_FETCH_LIMIT` | Liberado feature | LOW | Remove or rename |
| `VAULT_PATH_TRUNCATE` | Liberado concept | LOW | Rename `PATH_TRUNCATE` |
| `SIDEBAR_STATUS_HEIGHT` | Liberado panel | MEDIUM | Configurable |
| `SIDEBAR_REACTIONS_MIN_HEIGHT` | Liberado panel | MEDIUM | Configurable |
| All others | Generic | NONE | Keep |

---

## What stays as-is (10 modules, zero changes)

These modules have no Liberado dependency and are immediately portable:

| Module | Content | Reason |
|--------|---------|--------|
| `sse.rs` | `SseDecoder`, `SseEvent` | Pure SSE parser, no backend references |
| `terminal.rs` | `TerminalGuard` | Pure crossterm/ratatui lifecycle |
| `format.rs` | `relative_time`, `format_uptime`, `truncate_for_display`, `short_id`, `truncate_path`, `safe_truncate` | Pure formatting, depends only on `chrono` |
| `tuning.rs` (mostly) | Timing, scroll, layout ratios, truncation limits | Only 4 of ~30 constants are Liberado-specific |
| `render/input.rs` | Input area rendering | Reads only `app.input`, `app.cursor`, `app.focus`, `app.streaming`, `app.theme` |
| `render/chat.rs` (mostly) | Chat pane rendering | Message enum is generic; tool rendering style portable |
| `handlers/input.rs` | Text input handler | Enter, backspace, cursor, escape, tab — fully generic |
| `handlers/chat.rs` | Chat message navigation | j/k up/down, enter expand/collapse — generic |
| `handlers/mouse.rs` | Mouse click/scroll | Uses `LayoutRects` and `point_in_rect` — generic |
| `conversations.rs` | Tree builder/flattener | Works on any `ConvHeader`-like with `parent_conversation` |
| Word boundary utils | `prev_word_boundary`, `next_word_boundary` | Pure functions, no deps |

Crate-level: `liberado-theme` and `liberado-markdown` already have zero Liberado dependency.

---

## Proposed crate architecture

```
agent-tui-core/
├── src/
│   ├── lib.rs              — re-exports; top-level docs
│   ├── app.rs              — GenericApp<B: Backend> state machine
│   ├── effects.rs          — EffectRunner with trait-based dispatch
│   ├── sse.rs              — SseDecoder, SseEvent (generic)
│   ├── terminal.rs         — TerminalGuard (generic)
│   ├── format.rs           — formatting utils (generic)
│   ├── conversations.rs    — tree utils (generic)
│   ├── tuning.rs           — tuning constants (generic subset)
│   ├── commands.rs         — CommandRegistry trait + built-in commands
│   ├── handlers/           — input.rs, chat.rs, sidebar.rs, mouse.rs (generic)
│   ├── render/
│   │   ├── mod.rs          — layout dispatch (configurable)
│   │   ├── chat.rs         — chat pane renderer
│   │   ├── input.rs        — input area renderer
│   │   ├── status_bar.rs   — status bar renderer
│   │   ├── sidebar_conversations.rs — conversation tree renderer
│   │   └── sidebar.rs      — generic sidebar container
│   └── traits.rs           — Backend, ChatProvider, ConversationProvider, StatusProvider, etc.
├── Cargo.toml
│   dependencies: ratatui, crossterm, tokio, serde, chrono, parking_lot, futures, liberado-theme, liberado-markdown

agent-tui-liberado/
├── src/
│   ├── lib.rs
│   ├── chat_provider.rs       — LiberadoChatProvider (api::post_chat_stream + SseDecoder mapping)
│   ├── conversation_provider.rs — LiberadoConversationProvider
│   ├── status_provider.rs     — LiberadoStatusProvider
│   ├── reaction_provider.rs   — LiberadoReactionProvider (optional)
│   ├── status_panel.rs        — Liberado-specific status panel (DaemonStatus rendering)
│   ├── reactions_panel.rs     — Liberado-specific reactions panel (ReactionEvent rendering)
│   ├── commands.rs            — /status, /fork, /model, /session (Liberado-specific commands)
│   ├── types.rs               — DaemonStatus, ReactionEvent, ConvHeader, ChatMessage (API types)
│   └── main.rs                — Binary entry point wiring core + Liberado providers
├── Cargo.toml
│   dependencies: agent-tui-core, reqwest, serde, serde_json, tokio, tracing

liberado-theme/        (no changes)
liberado-markdown/     (no changes)
```

### Dependency tree

```
agent-tui-liberado
  ├── agent-tui-core
  │     ├── liberado-theme (agnostic)
  │     └── liberado-markdown (agnostic)
  ├── reqwest
  └── serde
```

Third-party backends would follow the same pattern: `agent-tui-openai`, `agent-tui-ollama`, etc.

---

## Trait surface

### Backend

```rust
/// The top-level trait connecting a backend's types to the core state machine.
pub trait Backend: Send + Sync + 'static {
    type Action: Clone + Send + Debug;
    type Effect: Send + Debug;
    type ConvSummary: Clone + Send + Debug;
    type StatusData: Clone + Send + Debug;

    fn conv_id(summary: &Self::ConvSummary) -> &str;
    fn conv_title(summary: &Self::ConvSummary) -> &str;
    fn conv_created_at(summary: &Self::ConvSummary) -> &str;  // ISO 8601
    fn conv_parent_id(summary: &Self::ConvSummary) -> Option<&str>;
}
```

### ChatProvider

```rust
/// Post a message and receive a streaming byte stream (typically SSE).
#[async_trait]
pub trait ChatProvider: Send + Sync {
    async fn post_chat_stream(
        &self,
        message: &str,
        session: Option<&str>,
    ) -> Result<Box<dyn ByteStream>, ChatError>;

    fn name(&self) -> &str;
}
```

### ConversationProvider

```rust
/// List and retrieve conversation history.
#[async_trait]
pub trait ConversationProvider<B: Backend>: Send + Sync {
    async fn list(&self) -> Result<Vec<B::ConvSummary>, Error>;
    async fn get(&self, id: &str) -> Result<Vec<GenericMessage>, Error>;
}
```

### StatusProvider

```rust
/// Fetch backend health/status information.
#[async_trait]
pub trait StatusProvider<B: Backend>: Send + Sync {
    async fn fetch(&self) -> Result<Option<B::StatusData>, Error>;
    fn is_available(&self, status: &B::StatusData) -> bool;
}
```

### ReactionProvider (optional)

```rust
/// Optional event/reaction feed for the sidebar.
#[async_trait]
pub trait ReactionProvider: Send + Sync {
    type Reaction: Clone + Send + Debug;

    async fn fetch(&self, limit: usize) -> Result<Vec<Self::Reaction>, Error>;
    fn render(&self, reaction: &Self::Reaction) -> String;
}
```

### CommandHandler

```rust
/// Registerable slash command.
pub trait CommandHandler<B: Backend>: Send {
    fn names(&self) -> Vec<&'static str>;
    fn handle(&self, app: &mut GenericApp<B>, args: &str) -> Vec<GenericEffect<B>>;
}
```

### GenericApp and type-erased enums

The `Action` and `Effect` enums become generic:

```rust
pub enum GenericAction<B: Backend> {
    // Always available (built-in)
    SseToken(String),
    SseTool { name: String, args: String },
    SseToolResult { name: String, ok: bool, preview: String },
    SseDone,
    SseFailed(String),
    ConnectionStatus(bool),
    Tick,
    ConversationsUpdate(Vec<B::ConvSummary>),
    HistoryLoaded { id: String, messages: Vec<ChatMessage> },
    SseSession(String),
    StatusUpdate(B::StatusData),
    // Extensible
    Custom(B::Action),
}

pub enum GenericEffect<B: Backend> {
    CancelStream,
    SetWindowTitle(String),
    Quit,
    None,
    Custom(B::Effect),
}
```

The `ReactionsUpdate` variant is removed since it's an optional plugin.

---

## Estimated effort

| Phase | Work | Days |
|-------|------|------|
| 1 | Trait extraction + `App<B: Backend>` generic | 3-4 |
| 2 | Provider traits + effect decoupling + Liberado impl | 2-3 |
| 3 | Render decoupling + command registry + configurable layout | 2-3 |
| 4 | Cleanup, docs, example backends (mock, echo, OpenAI) | 2 |
| **Total** | | **7-10** |

The work is systematic and well-bounded because the existing architecture already cleanly
separates concerns — it just hardcodes the Liberado-specific types and endpoints at the
trait boundary rather than behind a trait. The SSE parser, input handlers, terminal lifecycle,
formatting utilities, theme integration, and markdown rendering require **zero changes**.
