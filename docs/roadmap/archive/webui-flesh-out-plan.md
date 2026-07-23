# WebUI Flesh-Out â€” Implementation Plan

**Status**: All 5 phases implemented (sidebar, collapsible thinking steps, MCP panel, markdown
rendering, slash commands) â€” see `crates/webui/src/components/{sidebar,mcp_panel,markdown,slash_commands}.rs`.
Kept as a design reference; not a live TODO list.

## Overview

The current WebUI (`crates/webui/`) has a working Dioxus shell with Chat and Status tabs, SSE
streaming, demo seed messages, and a styling system. This plan fleshes it out into a production chat
interface with: a **sidebar** (conversation history + MCP catalog), **collapsible thinking steps**
(tool calls collapsed by default), and a **text input box** (already exists; we enhance it).

## Architecture â€” post-flesh-out layout

```
â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”¬â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
â”‚  SIDEBAR     â”‚  MAIN CONTENT                                    â”‚
â”‚              â”‚                                                  â”‚
â”‚  â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â” â”‚  â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â” â”‚
â”‚  â”‚ New Chat â”‚ â”‚  â”‚  Chat History (messages panel)              â”‚ â”‚
â”‚  â”œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”¤ â”‚  â”‚                                            â”‚ â”‚
â”‚  â”‚ Conv 1   â”‚ â”‚  â”‚  â”Œâ”€ user bubble                            â”‚ â”‚
â”‚  â”‚ Conv 2   â”‚ â”‚  â”‚  â”œâ”€ assistant bubble                       â”‚ â”‚
â”‚  â”‚ Conv 3   â”‚ â”‚  â”‚  â”‚  â”œâ”€ thinking step â–¸ (collapsed)         â”‚ â”‚
â”‚  â”‚ ...      â”‚ â”‚  â”‚  â”‚  â”‚  â””â”€ ðŸ”§ search-memory âœ“ 3 results     â”‚ â”‚
â”‚  â”œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”¤ â”‚  â”‚  â”‚  â””â”€ final answer                        â”‚ â”‚
â”‚  â”‚ MCP       â”‚ â”‚  â”‚  â””â”€ ...                                   â”‚ â”‚
â”‚  â”‚ servers   â”‚ â”‚  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜ â”‚
â”‚  â”‚ (collaps.)â”‚ â”‚  â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â” â”‚
â”‚  â”‚          â”‚ â”‚  â”‚  Input Bar  [________________] [Send] [â¹] â”‚ â”‚
â”‚  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜ â”‚  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜ â”‚
â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”´â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
```

## Phases

Each phase is a self-contained PR that can be reviewed independently.

---

### Phase 1 â€” Sidebar + Layout Restructuring

**Goal:** Convert the single-column layout into a sidebar + main-content split. Add the conversation
history list to the sidebar.

**Files touched:**
- `crates/webui/src/main.rs` â€” restructure layout to sidebar + main
- `crates/webui/src/components/mod.rs` â€” add `sidebar` module
- `crates/webui/src/components/sidebar.rs` â€” new: sidebar component with conversation list
- `crates/webui/src/styles/main.css` â€” sidebar styles, layout changes

**Details:**

1. **Layout** (`main.rs`):
   - Replace current vertical layout with a horizontal split: `.app-layout { display: flex; }`
   - Left: `.sidebar` (280px, collapsible via a toggle button, border-right)
   - Right: `.main-content` (flex: 1, contains chat + status views as before)
   - Move the nav (Chat / Status tabs) inside the main-content header
   - Sidebar toggle button in the top bar

2. **Sidebar component** (`components/sidebar.rs`):
   - `pub fn Sidebar(api_base, active_conv_id, on_select_conv, on_new_chat)` 
   - Fetches `GET /api/conversations` on mount (and every 30s or on focus)
   - Renders list of `ConvHeader` items: title (or "Untitled" fallback), timestamp
   - Active conversation highlighted with `--lib-sidebar-selected-bg`
   - Click calls `on_select_conv(id)` callback; `on_new_chat` starts fresh
   - Empty state: "No conversations yet" message
   - Each item shows truncated title + relative timestamp (e.g., "2m ago")

3. **Chat component changes** (`chat.rs`):
   - Accept `conv_id: Option<String>` prop â€” when set, loads `GET /api/conversations/{id}` on mount
   - When `None`, starts fresh (demo seed removed; empty state)
   - `ChatMsg` struct stays as-is for this phase
   - Send a `session` query param on subsequent turns

4. **CSS** (`main.css`):
   - `.app-layout` flex container
   - `.sidebar` styles (width, border-right, overflow-y scroll, bg from `--lib-sidebar-item-bg`)
   - `.sidebar-header` with "New Chat" button
   - `.conv-item` / `.conv-item.active` / `.conv-item-title` / `.conv-item-time`
   - `.sidebar-toggle` button styles
   - Remove top-level `.chat { height: â€¦ }`, replace with `.main-content { display: flex; flex-direction: column; height: 100vh; }` so the chat fills the right pane

5. **Stop button** â€” add a Stop (â¹) button next to Send in the input bar. During streaming (`sending() == true`), clicking it closes the EventSource and sets `sending(false)`. This wires up the backend stop/cancel primitive already documented in `docs/reference/api.md`.

**Verification:** `dx serve` shows sidebar on left, chats load, clicking a conversation loads its history, Stop button works mid-stream.

---

### Phase 2 â€” Collapsible Thinking Steps

**Goal:** Tool calls that happen during a turn are grouped as "thinking steps" under the assistant
bubble, collapsed by default. Clicking expands to show tool args and result details.

**Files touched:**
- `crates/webui/src/components/chat.rs` â€” enhance `ChatMsg`, new `ThinkingStep` type, new `ThinkingStepView` component
- `crates/webui/src/styles/main.css` â€” thinking-step drop-down styles

**Design:**

```rust
/// A tool call + its result, grouped as a thinking step under an assistant message.
#[derive(Clone, PartialEq)]
struct ThinkingStep {
    tool_name: String,       // e.g., "search-memory"
    tool_args: String,       // e.g., '{"query": "hello"}'
    ok: Option<bool>,        // None = still running, Some(true) = success, Some(false) = fail
    preview: String,         // result preview (truncated by server)
}

/// Enhanced chat message with optional thinking steps.
#[derive(Clone, PartialEq)]
struct ChatMsg {
    role: &'static str,
    content: String,
    thinking_steps: Vec<ThinkingStep>,  // tool calls that preceded this message
}
```

**Streaming behavior:**
- On `tool` SSE event: create a new `ThinkingStep` with `ok: None`, append to the *last* assistant message's `thinking_steps` (or create an empty assistant placeholder first if none exists)
- On `tool_result` SSE event: find the matching `ThinkingStep` by tool name (most recent unresolved), set `ok` and `preview`
- On `token` SSE events: append to `content` as before
- Render: when an assistant message has `thinking_steps`, render them as a collapsible group between the message and the next user message

**Collapsible rendering:**
```
â”Œâ”€ assistant bubble â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
â”‚                                                  â”‚
â”‚  â–¸ Thinking (2 steps) â€” search-memory, read-file â”‚  â† collapsed
â”‚                                                  â”‚
â”‚  Here's what I found in your notes...            â”‚  â† assistant answer text
â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜

â–¼ Thinking (2 steps)                              â† expanded
  â”Œâ”€ ðŸ”§ search-memory ("hello") â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
  â”‚  âœ“ Found 3 results in vault                   â”‚
  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
  â”Œâ”€ ðŸ”§ read-file (notes.md) â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
  â”‚  âœ“ Read 1,234 bytes                           â”‚
  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
```

- Clicking the "â–¸ Thinking" header toggles between `â–¸` (collapsed) and `â–¼` (expanded)
- When expanded, each step shows tool name, args, and result preview
- While any tool is still running (`ok: None`), show a spinner on that step and auto-expand
- When all tools complete, collapse after a 500ms delay (or keep expanded if user manually expanded)

**CSS:**
- `.thinking-group` â€” container with subtle border, rounded, slightly inset from bubble
- `.thinking-header` â€” clickable row with arrow indicator, tool count, tool name summary
- `.thinking-step` â€” individual tool call row (compact monospace)
- `.thinking-step.pending` â€” with spinner or pulse animation
- `.thinking-step .tool-name` â€” yellow/accent color
- `.thinking-step .tool-result` â€” green (ok) or red (err)

---

### Phase 3 â€” MCP Connections Panel

**Goal:** Show the MCP (Model Context Protocol) servers registered with the daemon in the sidebar
or as a collapsible section. Shows each server's name, description, consequence level, tool count,
and tool names.

**Files touched:**
- `crates/webui/src/components/mcp_panel.rs` â€” new component
- `crates/webui/src/components/sidebar.rs` â€” integrate MCP panel as a section
- `crates/webui/src/styles/main.css` â€” MCP panel styles

**API:** `GET /api/catalog` returns `CatalogResponse { mcps: Vec<McpInfo> }` where:
```rust
McpInfo {
    name: String,
    description: String,
    consequence: String, // "read_only" | "reversible" | "irreversible" | "external"
    tool_count: usize,
    tool_names: Vec<String>,
    provenance: Option<String>,
}
```

**Rendering:**
```
â”Œâ”€ MCP Servers â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ â–¼ â”€â”
â”‚                                           â”‚
â”‚  â–¸ vault (reversible, 3 tools)            â”‚
â”‚    â”œâ”€ read                                â”‚
â”‚    â”œâ”€ write                               â”‚
â”‚    â””â”€ search                              â”‚
â”‚                                           â”‚
â”‚  â–¸ tasks-mcp (reversible, 2 tools)        â”‚
â”‚    â”œâ”€ add                                 â”‚
â”‚    â””â”€ list                                â”‚
â”‚                                           â”‚
â”‚  â–¸ deepwiki (read_only, 1 tool)           â”‚
â”‚    â””â”€ ask_question                        â”‚
â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
```

- Header "MCP Servers" with toggle to collapse/expand the whole section
- Each MCP server shows as a collapsible row with:
  - Name (bold), consequence badge (color-coded: green=read_only, yellow=reversible, red=irreversible/or external), tool count
  - Click expands to show tool names in a monospace list
- Empty state when no MCPs are registered: "No MCP servers registered"
- Fetches on mount and on a 60s polling interval (or on window focus)

**Consequence badge colors:**
- `read_only` â†’ `--lib-reaction-observed` (cyan)
- `reversible` â†’ `--lib-reaction-acted` (green)
- `irreversible` / `external` â†’ `--lib-tool-err` (red)

**CSS additions:**
- `.mcp-panel` â€” matches card style
- `.mcp-server` â€” row with name + badge + tool count
- `.mcp-server-header` â€” clickable, expand/collapse
- `.mcp-tool-list` â€” indented monospace list
- `.mcp-tool` â€” individual tool name
- `.consequence-badge` â€” pill-shaped colored indicator

---

### Phase 4 â€” Markdown Rendering (stretch)

**Goal:** Render assistant/user messages as HTML from Markdown using `pulldown-cmark`.

**Files touched:**
- `crates/webui/Cargo.toml` â€” add `pulldown-cmark`
- `crates/webui/src/components/markdown.rs` â€” new: markdown-to-Dioxus-Element renderer
- `crates/webui/src/components/chat.rs` â€” use markdown renderer for assistant/user bubbles
- `crates/webui/src/styles/main.css` â€” markdown element styles (code blocks, lists, links, etc.)

### Phase 5 â€” Slash Commands (stretch)

**Goal:** Wire the scaffolded `slash_commands.rs` into the chat input.

**Files touched:**
- `crates/webui/Cargo.toml` â€” move `liberado-commands` into wasm32 deps
- `crates/webui/src/components/mod.rs` â€” add `slash_commands` module
- `crates/webui/src/components/chat.rs` â€” integrate slash command handling in submit
- `crates/webui/src/components/slash_commands.rs` â€” make it compile and work

---

## Order of Implementation

1. **Phase 1 first** â€” it restructures the layout, which everything else builds on
2. **Phase 2 second** â€” the thinking steps are the most visible new feature
3. **Phase 3 third** â€” MCP panel slots into the sidebar built in Phase 1
4. **Phase 4 optionally** â€” markdown is nice-to-have, not core to the UI structure
5. **Phase 5 optionally** â€” slash commands are already scaffolded

## Verification

After each phase:
```powershell
# Start the daemon (API only â€” no need to rebuild the wasm bundle for hot-reload dev)
.\scripts\start-daemon.ps1 -VaultPath <path>

# Start the dev server (hot reload)
.\scripts\start-webui-dev.ps1

# Open http://localhost:8080 and test the feature

# When done:
.\scripts\stop-webui-dev.ps1
.\scripts\stop-daemon.ps1
```

(`start-webui.ps1` / `stop-webui.ps1` remain for the non-hot-reload path: build the wasm bundle once
and have the daemon serve it statically.)

---

## Goal-session view â€” and why it is load-bearing beyond the WebUI

The WebUI has `chat.rs` and **no goal-session view at all**: sessions are browsable only in the TUI.
That is a gap in its own right, but it also blocks something concrete.

A goal session can now **stop mid-build and ask you a question**, ping you out-of-band when nobody is
watching, and wait for hours (one-execution-engine E5). The ping reaches your phone. You then cannot
answer it â€” the alert says *"answer in the TUI or via `POST /api/goals/{id}/message`"*, and a reply typed
into Telegram goes nowhere (confirmed in the live run, 2026-07-14).

The intended fix is a **deep link**: the alert carries a URL to the session on the homelab instance, you
tap it, and you answer in a view that shows the question *and the context around it* â€” the transcript,
what the pack tried, the diff so far. A Telegram reply bridge could deliver the text, but it is a
keyhole: you would be answering a question you cannot see the reasons for.

So this view is what turns "the daemon will wait 6 hours for you" into a thing you can actually use.

Needs, in order:

1. **A session view** â€” read the transcript + events (`GET /api/goals/{id}`, or the SSE stream for live),
   render an `awaiting_input` prompt with its `options`, and `POST â€¦/message` to answer. The API already
   exists and the TUI already consumes it; nothing new is needed server-side.
2. **`public_base_url` in config**, so `NotifySessionAlert::session_needs_you`
   (`crates/server/src/lib.rs`) can compose the link. **Do not add this key before the page exists** â€”
   an unused config key is a lie about what the system can do.

See [one-execution-engine-live-test.md](one-execution-engine-live-test.md) section E5-b.
