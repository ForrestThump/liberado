# ACP Bridge — Completion Roadmap

**Status:** Skeleton landed (`crates/acp-bridge/`). Emits placeholder text; needs chat-engine wiring.

**Related:** Paseo provider at `paseo/packages/server/src/server/agent/providers/liberado-acp-agent.ts` (30 lines, extends `GenericACPAgentClient`).

---
## What works today

- NDJSON framing on stdin/stdout
- ACP handshake (`initialize` → protocol version + capabilities)
- Session create/resume (`newSession`, `loadSession` → session ID)
- ACP error protocol (malformed messages, unknown methods)
- Paseo provider registration (binary spawn + lifecycle management inherited from `ACPAgentClient`)

## What needs wiring — in order

### 1. Provider resolution (1–2 hours)

The bridge needs to create an `OpenAiCompatibleProvider` from the environment, the same way `liberado-coder-run` does.

**File:** `crates/acp-bridge/src/main.rs`  
**What to do:**
- Read `DEEPSEEK_API_KEY` (or `OPENROUTER_API_KEY`) from the environment
- Construct `OpenAiCompatibleProvider` with the model name `deepseek-chat` (or from `LIBERADO_ACP_MODEL` env var)
- Wrap it in `Arc<dyn Provider>`
- Optionally: read a `topology.toml` config path from `LIBERADO_CONFIG_DIR` or `--config-dir`

**Dependencies to add:** `liberado-provider-openai-compat`, `liberado-config-loader` (already in Cargo.toml; remove unused ones like `liberado-conversation-store`)

### 2. Chat session lifecycle (2–3 hours)

The bridge needs to create and manage conversations using `liberado-main-agent`'s `ChatSession`/`Conversation` types.

**File:** `crates/acp-bridge/src/main.rs`  
**What to do:**
- Store a `HashMap<String, ChatSession>` keyed by ACP session ID
- On `newSession`: create a new `ChatSession` via `ConversationStore::create()`
- On `loadSession`: load an existing session from the conversation store
- Store sessions in a `HashMap<String, Arc<Mutex<ChatSession>>>` or similar
- On `prompt`: route to the session's `process_turn()` method
- On `cancel`: call the session's abort mechanism
- On session close/process exit: clean up sessions

**Key types to use:**
```rust
use liberado_main_agent::sessions::ChatSessions;
use liberado_conversation_store::ConversationStore;
```

**State to track per session:**
```rust
struct AcpSession {
    conversation_id: String,
    sessions: Arc<ChatSessions>,
    provider: Arc<dyn Provider>,
}
```

### 3. Event streaming (3–4 hours)

The hardest part: converting Liberado's `AgentEvent` stream into ACP `agentMessage` notifications.

**Liberado's `AgentEvent` enum** (`crates/executor/src/lib.rs:50`):
```rust
pub enum AgentEvent {
    Token(String),                                              // text delta
    ToolStarted { name: String, args: String },                // tool call starting
    ToolFinished { name: String, ok: bool, preview: String },  // tool result
    Done,                                                       // answer complete
    Error(String),                                             // failure
}
```

**ACP `agentMessage` notification format:**
```json
{
  "method": "agentMessage",
  "params": {
    "sessionId": "...",
    "message": {
      "id": "...",
      "role": "assistant",
      "parts": [
        { "type": "text", "text": "..." },
        { "type": "tool_call", "id": "...", "name": "...", "input": {...} },
        { "type": "tool_result", "id": "...", "name": "...", "output": "..." }
      ]
    }
  }
}
```

**Mapping:**
| `AgentEvent` | ACP part type | Notes |
|---|---|---|
| `Token(text)` | `{ type: "text", text }` | Accumulate consecutive Tokens into one text part |
| `ToolStarted` | `{ type: "tool_call", status: "running" }` | Emit immediately |
| `ToolFinished` | `{ type: "tool_result" }` | Emit after the tool completes |
| `Done` | `stopReason: "end_turn"` | Final response to `prompt` request |
| `Error(msg)` | `{ type: "text", text: "Error: {msg}" }` | Inline error |

**Implementation approach:**
```rust
async fn process_prompt(
    session: &mut AcpSession,
    text: &str,
) -> Result<(), String> {
    // 1. Start a turn on the chat session
    let (replay, mut live_rx) = session.sessions
        .start_or_attach(session.conversation_id, &text)
        .await?;

    // 2. Replay prior events (tools from interrupted turns, etc.)
    for event in replay {
        emit_acp_notification(&session.id, &event)?;
    }

    // 3. Stream live events
    while let Some(event) = live_rx.recv().await {
        emit_acp_notification(&session.id, &event)?;
        if matches!(event, AgentEvent::Done | AgentEvent::Error(_)) {
            break;
        }
    }

    Ok(())
}
```

### 4. Cancel / interrupt (1 hour)

Wire the `cancel` ACP method to Liberado's turn abort.

**What to do:**
- Store an `AbortHandle` or `tokio::sync::watch` sender per session
- On `cancel`: signal the abort, which causes the executor loop to terminate
- Emit a `turn_canceled`-equivalent notification

### 5. Provider catalog (1 hour)

Wire `fetchCatalog` in the Paseo provider to return real model lists.

**What to do:**
- In `LiberadoACPAgentClient.fetchCatalog()`: spawn `liberado-acp`, send an `initialize` message, parse the capabilities response
- Or: hardcode the model list in the provider (simpler, matches what other providers do)
- Report `deepseek-chat`, `deepseek-v4-pro`, and any models from topology.toml

### 6. Session persistence (2 hours)

Enable `resumeSession` in the Paseo provider to reconnect to an existing conversation.

**What to do:**
- Store `ConversationStore` backed by `<LIBERADO_DATA_DIR>/conversations/`
- On `loadSession`: look up the conversation by ID, verify it exists, create a session handle
- On `describePersistence()`: return the conversation ID so Paseo can store it

### 7. Permission mapping (optional, 2 hours)

Map Liberado's capability system to Paseo's permission model.

**What to do:**
- Liberado grants `AskHuman` for interactive sessions — this maps to Paseo's `permission_requested` + `respondToPermission`
- Liberado gates writes through `PathPolicy` and `CommandPolicy` — these are transparent to Paseo
- For a v1, skip permission mapping entirely (Liberado handles permissions internally, Paseo doesn't need to mediate)

### 8. Mode support (optional, 1 hour)

Expose Liberado's plan/explore modes as Paseo modes.

**What to do:**
- In the provider definition, add modes: `"full"` (default), `"plan"` (read-only plan artifact), `"explore"` (read-only)
- On `setSessionMode`: toggle `PathPolicy` and tool catalog accordingly
- These are already implemented in Liberado's coding pack as presets

---
## Dependency cleanup

The current `Cargo.toml` for `liberado-acp-bridge` declares dependencies that aren't used yet:

| Dependency | Used? | Action |
|---|---|---|
| `liberado-main-agent` | ❌ | Needed for #2–#3 (chat sessions) |
| `liberado-provider` | ❌ | Needed for #1 (Provider trait) |
| `liberado-provider-openai-compat` | ❌ | Needed for #1 (real model calls) |
| `liberado-config-loader` | ❌ | Needed for #1 (topology.toml) |
| `liberado-conversation-store` | ❌ | Needed for #6 (persistence) |
| `liberado-common` | ❌ | Needed for #2 (shared types) |

None are currently used — the skeleton just reads JSON and echoes placeholder text. Remove unused ones until #1–#2 are implemented, or keep them (they'll be needed soon).

---
## Testing strategy

1. **Unit: ACP message parsing** — serialize/deserialize round-trip for all message types
2. **Integration: mock provider** — wire `MockProvider` through the ACP bridge, send a prompt, verify `agentMessage` notifications contain scripted tool calls
3. **End-to-end: Paseo smoke** — register the provider in a local Paseo daemon, send a prompt through the Paseo UI, verify the response appears
4. **Regression: harness-bench** — run `liberado-coder-run task run` to ensure nothing broke

---
## Rough effort estimate

| # | Task | Hours |
|---|---|---|
| 1 | Provider resolution | 1–2 |
| 2 | Chat session lifecycle | 2–3 |
| 3 | Event streaming (AgentEvent → ACP) | 3–4 |
| 4 | Cancel / interrupt | 1 |
| 5 | Provider catalog | 1 |
| 6 | Session persistence | 2 |
| 7 | Permission mapping | 2 (optional) |
| 8 | Mode support | 1 (optional) |
| **Total** | **10–14 hours (core: 8–11)** | |
