# Shared Wire-Type Extraction Plan

**Status (2026-07-05)**: Steps 0–3 (repointing server/TUI/WebUI at `chat-client-contract`'s shared
DTOs and `ChatEvent::from_sse_data()`) and the `SseDecoder`/`SseEvent` half of Step 4 are **done** —
confirmed by [`crate-modularity-audit.md`](crate-modularity-audit.md) finding 2, which independently
re-verified this while auditing crate coupling. The section 8 checkboxes below were the original
targets and are left unedited as the plan's record of intent; treat the modularity audit as the
up-to-date source on what's actually landed. **Resolved, not adopted**: this plan's proposed
`ChatClient` trait was never implemented by either client, and was deleted 2026-07-05 rather than
adopted — TUI and CLI's actual transport needs (blocking REPL vs. non-blocking render loop) diverge
too much for one shared `send`/`stream` trait to be worth forcing; `SseDecoder`/
`ChatEvent::from_sse_data` are the real, working shared boundary. See
`hygiene-audit-2026-07-05.md` P2.5 and `crate-modularity-audit.md` finding 2.

Promote `chat-client-contract` from an orphan crate to the single source of truth for all
wire DTOs shared between server, TUI, WebUI, and CLI. Fix type drift before it widens.

---

## 1. Motivation

Three clients (TUI, WebUI, CLI) call the same HTTP/SSE API (`docs/reference/api.md`), but every
endpoint type is defined independently — the TUI in `tui/src/api.rs`, the WebUI in
`webui/src/types.rs`, and the server in `server/src/state.rs` / inline `json!()` — and they
have already diverged. The `chat-client-contract` crate exists with the right doc-comment
("Every client depends only on this crate"), but today it only holds `ChatEvent`/`ChatClient`
and is not actually consumed by the server or the WebUI.

Extracting shared types now, before further WebUI work, eliminates the fork and makes every
endpoint a single typed contract.

### Divergences already present

| Field / Type | TUI (`tui/src/api.rs`) | WebUI (`webui/src/types.rs`) | Server (`server/src/`) |
|---|---|---|---|
| `DaemonStatus.uptime_seconds` | `u64` with `#[serde(default)]` | `Option<u64>` | always present as `u64` |
| `DaemonStatus.model_name` | `Option<String>` | absent | absent (not in `json!()`) |
| `DaemonStatus.token_usage_total` | `Option<u64>` | absent | absent |
| `DaemonStatus.context_window` | `Option<u64>` | absent | absent |
| `DaemonStatus.chat_tools` | `usize` | absent | present in `json!()` |
| `DaemonStatus.chat_tool_names` | `Vec<String>` | absent | present in `json!()` |
| `ReactionEvent.timestamp` | `String` | `DateTime<Utc>` | `String` |
| `ReactionEvent.outcome` | `String` | `ReactionOutcome` enum | `&'static str` |
| `ReactionOutcome` enum | absent | defined in `types.rs` | absent |
| `ConvHeader` | defined in `tui/api.rs` | absent | used via `sessions.list()` |
| `ChatMessage` | defined in `tui/api.rs` | absent | used via `sessions.history()` |
| `VaultInfo` | absent | defined in `types.rs` | constructed inline `json!()` |
| `ApiError` | absent | defined in `types.rs` | constructed inline `json!()` |
| SSE event parsing | `sse.rs` hand-rolls `{name, args}` / `{name, ok, preview}` via `v["name"].as_str()` | `chat.rs` hand-rolls same pattern in closures | `api.rs` uses `AgentEvent`, not `ChatEvent` |

---

## 2. Target Architecture

```
chat-client-contract/
├── Cargo.toml              # deps: serde only (wasm-clean core)
└── src/
    ├── lib.rs              # pub mod core; #[cfg(not(wasm32))] pub mod native;
    ├── core.rs             # all wire DTOs — zero native deps (just serde)
    └── native.rs           # ChatClient trait + SseDecoder — tokio/futures/async-trait
                             # gated behind #[cfg(not(target_arch = "wasm32"))]
```

**Consumers:**
- **Server** (`liberado-server`) — depends on `chat-client-contract` (core) for all response types. Replaces inline `json!()` and its own `ReactionEvent` with the shared types.
- **TUI** (`liberado-tui`) — depends on `chat-client-contract` (core + native). Deletes `api.rs` DTO duplicates. SseDecoder's `to_action()` uses `ChatEvent` instead of hand-rolled JSON parsing.
- **WebUI** (`liberado-webui`) — depends on `chat-client-contract` (core only). Deletes `types.rs` DTOs. SSE parsing uses `ChatEvent` deserialization instead of hand-rolled closures.
- **CLI** (`liberado-cli`) — already depends on `chat-client-contract`.

**What stays put:**
- `SseDecoder` (the incremental string-framing parser) — native transport, stays in `tui/src/sse.rs` (or moves to `chat-client-contract/src/native.rs`). Not shared with WebUI (browser `EventSource` handles framing).
- `reqwest` HTTP client functions (`fetch_status`, `post_chat_stream`, etc.) — native transport, stays in `tui/src/api.rs`.
- `EventSource` integration in `webui/chat.rs` — browser transport, stays put.
- All rendering code (`app.rs`, `render/*`, `format.rs`, Dioxus components).
- `liberado-commands` slash-command logic — native-only today; WASM compatibility is out of scope.

---

## 3. Reconciled Types

### `DaemonStatus` — superset of both clients

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub vault_path: String,
    #[serde(default)]
    pub uptime_seconds: u64,        // TUI: u64 with default; server always emits it
    pub watcher_active: bool,
    pub dispatcher_attached: bool,
    pub orchestrator_attached: bool,
    pub reactions_seen: u64,
    #[serde(default)]
    pub model_name: Option<String>,     // TUI forward-looking; absent → None
    #[serde(default)]
    pub token_usage_total: Option<u64>, // TUI forward-looking; absent → None
    #[serde(default)]
    pub context_window: Option<u64>,    // TUI forward-looking; absent → None
    #[serde(default)]
    pub chat_tools: usize,              // TUI + server; absent → 0
    #[serde(default)]
    pub chat_tool_names: Vec<String>,   // TUI + server; absent → empty
}
```

### `ReactionEvent` — typed outcome

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionEvent {
    pub event_type: String,
    pub timestamp: String,        // ISO-8601 string (no chrono dependency in protocol crate)
    pub source: String,
    pub correlation_id: String,
    pub path: Option<String>,
    pub outcome: ReactionOutcome, // typed enum, not String / &'static str
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReactionOutcome {
    Observed,
    Decided,
    Acted,
}
```

`timestamp` stays `String` so the protocol crate avoids a `chrono` dependency. WebUI already
parses it into `DateTime<Utc>` for display — that conversion moves into `reactions.rs`'
`ReactionRow` component (one `DateTime::parse_from_rfc3339` call).

**⚠️ Wire format change — `outcome` string values will change.** The server's current
output uses `reaction.outcome.label()` which produces `"(observed)"` (with parentheses!),
arbitrary action labels for `Decided`, and `"acted:reported"` / `"acted:clarify"` /
`"acted:proposed"` for `Acted`. The new enum serializes as `"observed"`, `"decided"`,
`"acted"`. This is not backward-compatible — any client not updated simultaneously would
see deserialization failures. Step 1 handles the server change; the three `Acted`
sub-variants are intentionally collapsed into a single `Acted` (matching the WebUI's
existing simplification). If the distinction between `acted:reported`, `acted:clarify`,
and `acted:proposed` is needed on the wire later, re-add sub-enum variants.

### `ConvHeader` — from TUI, promoted

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConvHeader {
    pub id: String,
    pub title: String,
    pub created_at: String,
    #[serde(default)]
    pub parent_conversation: Option<String>,
    #[serde(default)]
    pub spawned_by: Option<String>,
}
```

### `ChatMessage` — from TUI, promoted

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}
```

### `VaultInfo` — from WebUI, promoted

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultInfo {
    pub root: String,
    pub note_count: u64,
    pub watcher_active: bool,
}
```

### `ApiError` — from WebUI, promoted

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
```

### `ChatEvent` — already exists, no change

The existing `ChatEvent` enum (already in `chat-client-contract`) is correct and maps 1:1 with
the server's SSE wire format. No change needed.

### `CatalogResponse` / `McpInfo` — already exist, no change

Already in `chat-client-contract`. Server currently constructs `catalog` as inline `json!()` —
it will instead serialize `CatalogResponse`.

---

## 4. Implementation Steps

### Step 0: Prepare `chat-client-contract`

**Goal:** Restructure the crate into `core` (wasm-clean DTOs) and `native` (ChatClient trait +
SseDecoder), with all divergent types reconciled.

**Cargo.toml changes:**
- `core` dependencies: `serde` only (already a workspace dep).
- `native` dependencies: `async-trait`, `tokio`, `futures`, `ulid`, `thiserror`, `serde_json`
  — gated behind `#[cfg(not(target_arch = "wasm32"))]`.
- Remove `async-trait`, `tokio`, `futures`, `ulid`, `thiserror` from the base `[dependencies]`
  and move them to `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`.

**File structure:**
1. Create `src/core.rs` with all wire DTOs:
   - `DaemonStatus`, `ReactionEvent`, `ReactionOutcome`, `ConvHeader`, `ChatMessage`,
     `VaultInfo`, `ApiError`, `ChatEvent`, `ChatResponse`, `McpInfo`, `CatalogResponse`,
     `ChatError` (error enum — used by `ChatClient` trait but defined in core so clients
     can reference it without pulling in native deps. **Note:** `ChatError` currently has
     `#[derive(Debug, thiserror::Error)]` without `Serialize`/`Deserialize`, so it needs
     `thiserror` in `core`'s dependencies, adding a small dep beyond `serde`. If the goal
     is truly zero native deps for core, consider keeping `ChatError` in `native.rs` instead.)
2. Create `src/native.rs` with:
   - `ChatClient` trait (gated `#[cfg(not(target_arch = "wasm32"))]`)
   - `SseDecoder` / `SseEvent` (moved from `tui/src/sse.rs`; rename `SseEvent` to avoid
     collision with the axum SSE Event type if needed — keep `SseEvent` as-is, it's fine)
3. Update `src/lib.rs`:
   ```rust
   pub mod core;
   #[cfg(not(target_arch = "wasm32"))]
   pub mod native;
   // Re-export everything for convenience
   pub use core::*;
   ```

**Tests:**
- Move existing `lib.rs` tests into `core.rs` tests module.
- Add round-trip tests for every new DTO (DaemonStatus, ReactionEvent with enum, ConvHeader,
  ChatMessage, VaultInfo, ApiError).
- Add a test that `DaemonStatus` deserializes both the server's current `json!()` shape and
  the TUI's extended shape (missing fields → defaults).
- Add a test that `ReactionEvent` deserializes from the **new** wire format (where
  `outcome` is `"observed"`, `"decided"`, or `"acted"`). **Do NOT** test against the
  server's current output — the current server sends `"(observed)"` (with parens),
  arbitrary action labels for `Decided`, and sub-variant strings like `"acted:reported"`
  which the enum cannot deserialize. See the wire-format warning in Section 3.
- Keep existing `SseDecoder` tests in `native.rs`.

**Verification:**
```powershell
cargo build -p chat-client-contract
cargo test -p chat-client-contract
```

---

### Step 1: Repoint the server at shared types

**Goal:** Server serializes all API responses using `chat-client-contract` types instead of
inline `json!()`.

**Changes:**

1. Add dependency in `crates/server/Cargo.toml`:
   ```toml
   chat-client-contract = { workspace = true }
   ```

2. **`server/src/state.rs`:**
   - Delete the local `ReactionEvent` struct.
   - Use `chat_client_contract::ReactionEvent` in `AppState.reactions` buffer.
   - Update `reaction_tx()` to construct `ReactionEvent { outcome: ReactionOutcome::X, ... }`
     instead of `outcome: reaction.outcome.label()` (which returns `&'static str`). Map the
     outcome labels to the enum:
     - `"observed"` → `ReactionOutcome::Observed`
     - `"decided"` → `ReactionOutcome::Decided`
     - `"acted"` → `ReactionOutcome::Acted`

3. **`server/src/api.rs`**
   - `status()`: return `Json(DaemonStatus { ... })` instead of `Json(json!({...}))`.
     **Note:** `DaemonStatus` now includes `model_name`, `token_usage_total`, and
     `context_window` as `Option<T>` (forward-looking fields from the TUI's `#[serde(default)]`
     definitions). The server does not currently populate them — they will serialize as
     `null` until the server adds the logic. Either accept `null` for now or add the
     population logic in this step.
   - `catalog()`: return `Json(CatalogResponse { mcps: ... })` instead of `Json(json!({...}))`.
     **Note on `McpInfo.tool_count` / `tool_names`:** `McpInfo` has these fields, but the
     server's `CapabilityCatalog::descriptors()` returns `McpDescriptor` structs that lack
     them (`name`, `description`, `consequence`, `provenance` only). The server needs a
     source for these per-MCP values — either wire them from the connected tool runtime, or
     populate them at catalog registration time, or accept `0`/`Vec::new()` as initial values
     and document this as a future improvement.
   - `vault()`: return `Json(VaultInfo { ... })` instead of `Json(json!({...}))`.
   - Error responses: return `Json(ApiError { error: ... })` instead of `Json(json!({"error":...}))`.
   - Chat responses: use `ChatResponse` and `ApiError` instead of inline `json!()`.

**Verification:**
```powershell
cargo build -p liberado-server
cargo test -p liberado-server
# Manual: start the server and curl /api/status, /api/catalog, /api/reactions
# to confirm output shape is identical (fields, order may differ but values match)
```

---

### Step 2: Repoint the TUI at shared types

**Goal:** Delete the DTO duplicates in `tui/src/api.rs`; use `chat-client-contract` types.
Keep the transport functions and `SseDecoder` but make parsing use `ChatEvent`.

**Changes:**

1. **`crates/tui/Cargo.toml`:** No change needed — already depends on `chat-client-contract`.

2. **`tui/src/api.rs`:**
   - Delete `DaemonStatus`, `ReactionEvent`, `ConvHeader`, `ChatMessage`, `ToolCallChip`,
     `ToolResultChip` structs.
   - Import from `chat_client_contract`:
     ```rust
     use chat_client_contract::{DaemonStatus, ReactionEvent, ConvHeader, ChatMessage};
     ```
   - Keep `ToolCallChip` and `ToolResultChip` if they are used as display-only types (they
     are constructed from `ChatEvent::Tool`/`ToolResult` data, not directly from JSON).
     If they duplicate `ChatEvent` fields, consider removing them and using `ChatEvent`
     variants directly in the render layer.
   - Keep all `fetch_*` and `post_chat_stream` functions — they return the shared types
     via `resp.json()` and the return types update automatically.
   - Update tests to use the shared types (import paths change; no behavior change).

3. **`tui/src/sse.rs`:**
   - `SseEvent::to_action()` — replace the hand-rolled `v["name"].as_str()` parsing with
     `serde_json::from_str::<ChatEvent>(&self.data)`:
     ```rust
     let event: ChatEvent = serde_json::from_str(&self.data)?;
     match event {
         ChatEvent::Session { id } => Ok(Action::SseSession(id)),
         ChatEvent::Token { text } => Ok(Action::SseToken(text)),
         ChatEvent::Tool { name, args } => Ok(Action::SseTool {
             name,
             args: args.to_string(),
         }),
         ChatEvent::ToolResult { name, ok, preview } => Ok(Action::SseToolResult { name, ok, preview }),
         ChatEvent::Done => Ok(Action::SseDone),
         ChatEvent::Failed { message } => Ok(Action::SseFailed(message)),
     }
     ```
   - This eliminates the `v["name"].as_str()` pattern and ensures parsing stays in sync
     with the protocol definition.
   - Note: the SSE `data` field for `tool`/`tool_result` events is a JSON object like
     `{"name":"search","args":"..."}`, which `ChatEvent::Tool` already handles via serde
     tag-based deserialization. However, `ChatEvent` uses `#[serde(tag = "type")]` and
     the SSE wire format does NOT include a `"type"` field in the data payload — the event
     type is in the SSE `event:` line. So we need a helper that deserializes without the
     tag:
     ```rust
     // In chat-client-contract core.rs, add:
     impl ChatEvent {
         /// Parse from an SSE data payload where the event type is already known
         /// from the SSE `event:` line (so no `"type"` field in the JSON).
         pub fn from_sse_data(event_type: &str, data: &str) -> Result<Self, serde_json::Error> {
             match event_type {
                 "session" => Ok(ChatEvent::Session { id: data.to_string() }),
                 "token" => Ok(ChatEvent::Token { text: data.to_string() }),
                 "tool" => {
                     #[derive(Deserialize)]
                     struct ToolPayload { name: String, args: serde_json::Value }
                     let p: ToolPayload = serde_json::from_str(data)?;
                     Ok(ChatEvent::Tool { name: p.name, args: p.args })
                 }
                 "tool_result" => {
                     #[derive(Deserialize)]
                     struct ToolResultPayload { name: String, ok: bool, preview: String }
                     let p: ToolResultPayload = serde_json::from_str(data)?;
                     Ok(ChatEvent::ToolResult { name: p.name, ok: p.ok, preview: p.preview })
                 }
                 "done" => Ok(ChatEvent::Done),
                 "failed" => Ok(ChatEvent::Failed { message: data.to_string() }),
                 _ => Err(serde::de::Error::custom(format!("unknown event type: {event_type}"))),
             }
         }
     }
     ```
   - Then `to_action()` becomes:
     ```rust
     pub fn to_action(&self) -> Result<Action, String> {
         ChatEvent::from_sse_data(&self.event, &self.data)
             .map(|ce| match ce {
                 ChatEvent::Session { id } => Action::SseSession(id),
                 ChatEvent::Token { text } => Action::SseToken(text),
                 ChatEvent::Tool { name, args } => Action::SseTool { name, args: args.to_string() },
                 ChatEvent::ToolResult { name, ok, preview } => Action::SseToolResult { name, ok, preview },
                 ChatEvent::Done => Action::SseDone,
                 ChatEvent::Failed { message } => Action::SseFailed(message),
             })
             .map_err(|e| format!("malformed SSE data ({e}): {}", self.data))
     }
     ```

4. **`tui/src/lib.rs`:** Update re-exports to reference `chat_client_contract` types.

**Verification:**
```powershell
cargo build -p liberado-tui
cargo test -p liberado-tui
```

---

### Step 3: Repoint the WebUI at shared types

**Goal:** Delete `webui/src/types.rs` DTOs. Use `chat-client-contract` (core) for types.
Replace hand-rolled SSE parsing in `chat.rs` with `ChatEvent::from_sse_data()`.

**Changes:**

1. **`crates/webui/Cargo.toml`:**
   - Add dependency:
     ```toml
     chat-client-contract = { workspace = true }
     ```

2. **`webui/src/types.rs`:** Delete the entire file (or keep as a re-export module).

3. **`webui/src/components/dashboard.rs`:**
   - Replace `use crate::types::DaemonStatus` with `use chat_client_contract::DaemonStatus`.
   - `model_name`, `token_usage_total`, `context_window` are new fields on `DaemonStatus`
     — update the component to display them (e.g. show model name if present, token count).
     The WebUI was missing these — this is a feature unification, not just a refactor.

4. **`webui/src/components/reactions.rs`:**
   - Replace `use crate::types::ReactionEvent` with `use chat_client_contract::ReactionEvent`.
   - Replace `use crate::types::ReactionOutcome` with `use chat_client_contract::ReactionOutcome`.
   - `timestamp` is now `String`, no longer `DateTime<Utc>`. Parse it for display:
     ```rust
     let time_str = chrono::DateTime::parse_from_rfc3339(&event.timestamp)
         .map(|dt| dt.format("%H:%M:%S").to_string())
         .unwrap_or_else(|_| event.timestamp.clone());
     ```

5. **`webui/src/components/chat.rs`:**
   - Import `use chat_client_contract::ChatEvent;`
   - Replace the hand-rolled `v["name"].as_str()` closures in the `tool` and `tool_result`
     event listeners with `ChatEvent::from_sse_data()`:
     ```rust
     // tool
     let on_tool = Closure::<dyn FnMut(MessageEvent)>::new(move |e: MessageEvent| {
         if let Some(data) = e.data().as_string() {
             if let Ok(ChatEvent::Tool { name, args }) = ChatEvent::from_sse_data("tool", &data) {
                 let args_str = args.to_string();
                 let label = if args_str.is_empty() || args_str == "{}" || args_str == "null" {
                     format!("🔧 {name}…")
                 } else {
                     format!("🔧 {name}({args_str})…")
                 };
                 messages.with_mut(|m| m.push(ChatMsg { role: "tool", content: label }));
             }
         }
     });
     ```
   - Similarly for `tool_result`, `session`, `token`, `done`, `failed`.
   - This eliminates the manual JSON field access and makes all SSE parsing single-source.

6. **`webui/src/main.rs`:** Update any `use crate::types::*` imports.

**Verification:**
```powershell
# Native desktop build
cargo build -p liberado-webui
# WASM build
cargo build -p liberado-webui --target wasm32-unknown-unknown
```

**WASM compatibility check:** `chat-client-contract`'s `core` module depends only on `serde`,
which compiles to `wasm32`. The `native` module (ChatClient trait, SseDecoder) is gated
behind `#[cfg(not(target_arch = "wasm32"))]`, so the WebUI never pulls in `tokio`/`futures`.
This means `cargo build --target wasm32-unknown-unknown` must succeed without native deps.

---

### Step 4: Update dependent crates & clean up

1. **`liberado-cli`:** Already depends on `chat-client-contract`. The CLI has its own
   private `SseDecoder` / `SseEvent` copy in `cli/src/chat_client.rs` (near-identical to
   the one in `tui/src/sse.rs`). Update it to:
   - Import `SseDecoder` from `chat_client_contract::native::SseDecoder` instead of
     maintaining a private copy.
   - Use `ChatEvent::from_sse_data()` instead of the hand-rolled `serde_json::from_str::<Value>`
     + `field(&v, "name")` pattern in `dispatch()`.
   - Delete the private `SseDecoder`, `SseEvent`, `parse_block`, and `strip_one_space`
     definitions.

2. **`Cargo.toml` (workspace root):** Verify `chat-client-contract` is listed in
   `[workspace.dependencies]` (it is, line 65).

3. **Remove dead code:**
   - `webui/src/types.rs` — delete entirely.
   - `tui/src/api.rs` — delete duplicate struct definitions; keep transport functions.
   - `server/src/state.rs` — delete local `ReactionEvent` struct.

4. **Full workspace build:**
   ```powershell
   cargo build --workspace
   cargo test --workspace
   ```

---

## 5. WASM Considerations

### What works today
- `serde` and `serde_json` compile cleanly to `wasm32`.
- The WebUI's browser-native `EventSource` handles SSE framing — no `SseDecoder` needed.
- The `ChatEvent::from_sse_data()` helper is pure deserialization (no async, no I/O).

### What is gated
- `ChatClient` trait (`Pin<Box<dyn Stream + Send>>`) — `Send` is not meaningful in WASM.
  Gated behind `#[cfg(not(target_arch = "wasm32"))]`.
- `SseDecoder` — string-framing parser for raw byte streams (reqwest). Gated same.
- `liberado-commands` — native-only today. WASM build of WebUI already has `#[cfg(not(wasm32))]`
  guards around slash-command handling (chat.rs:26,51). Making commands work in WASM is
  future work; the protocol crate extraction doesn't change this.

### Future: enabling `/commands` in WASM
`liberado-commands` needs a WASM audit (its deps may include native-only crates). If/when
that happens, it should follow the same pattern: pure command logic in a core module,
native I/O gated. Out of scope for this extraction.

---

## 6. What NOT to Extract

Per the analysis in the original brief, the following stay in their respective crates:

| Component | Why it stays |
|---|---|
| `tui/src/app.rs` (Action/update/App) | Ratatui Elm architecture; Dioxus uses signals |
| `tui/src/render/*` | Ratatui terminal rendering; not portable |
| `tui/src/input.rs` | Crossterm input handling; native only |
| `tui/src/conversations.rs` | TUI-specific state machine |
| `tui/src/format.rs` | Ratatui `Span` formatting |
| `tui/src/sse.rs` `SseDecoder` | String-framing incremental parser; native transport |
| `webui/components/chat.rs` `EventSource` usage | Browser-native SSE; different transport |
| `webui/components/*` Dioxus components | Dioxus signal model; different framework |
| `liberado-commands` | Deserves its own WASM audit, not part of this |

---

## 7. Execution Order & Risk

| Step | Risk | Rollback |
|---|---|---|---|
| 0. Restructure chat-client-contract | Low — additive; existing types preserved | Revert Cargo.toml + src changes |
| 1. Repoint server | **Medium-High** — `ReactionEvent.outcome` changes from arbitrary strings (e.g. `"(observed)"`, `"acted:reported"`) to a 3-variant enum (`"observed"`, `"decided"`, `"acted"`). Any client not updated in lockstep will fail to deserialize. `DaemonStatus` gains 3 new `Option` fields and `CatalogResponse` gains `tool_count`/`tool_names` with no server-side data source yet. | Verify with curl diff before/after (but expect `/api/reactions` output to change on purpose) |
| 2. Repoint TUI | Low — type aliases; behavior unchanged | Revert import paths |
| 3. Repoint WebUI | Medium — WASM build must succeed | Verify wasm32 build after each file |
| 4. Clean up & verify | Low — delete dead code; unify CLI's `SseDecoder` | Restore deleted files from git |

**Mitigations:**
- After Step 1, run curl against `/api/status`, `/api/vault`, `/api/reactions` before
  and after the change. `/api/status` and `/api/vault` must match field-for-field (modulo
  the new `model_name`/`token_usage_total`/`context_window` fields which now serialize as
  `null`). `/api/reactions` will have different `outcome` string values — this is expected.
- Update the server, TUI, WebUI, and CLI in the same deploy cycle to avoid wire-format
  incompatibility for `ReactionEvent.outcome`.

---

## 8. Success Criteria

- [ ] `chat-client-contract` has `core` module with all wire DTOs (zero native deps)
- [ ] `chat-client-contract` has `native` module with `ChatClient` + `SseDecoder` (gated)
- [ ] Server uses `chat-client-contract` types for all API responses (no more inline `json!()`)
- [ ] TUI imports DTOs from `chat-client-contract`; `api.rs` has no duplicate structs
- [ ] TUI SSE parsing uses `ChatEvent::from_sse_data()` (no hand-rolled `v["name"].as_str()`)
- [ ] WebUI imports DTOs from `chat-client-contract`; `types.rs` is gone
- [ ] WebUI SSE parsing uses `ChatEvent::from_sse_data()` (no hand-rolled closures)
- [ ] `cargo build --workspace` succeeds
- [ ] `cargo build -p liberado-webui --target wasm32-unknown-unknown` succeeds
- [ ] `cargo test --workspace` passes
- [ ] No duplicate definitions of `DaemonStatus`, `ReactionEvent`, `ReactionOutcome`,
  `ConvHeader`, `ChatMessage`, `VaultInfo`, `ApiError` exist in any crate
