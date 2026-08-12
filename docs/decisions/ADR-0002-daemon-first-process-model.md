---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0002
open_items: false
---

# ADR-0002: Daemon-First vs. TUI-First Process Model

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0002 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Daemon-first process model: the core agent loop, liberado dispatcher, MCP/hook clients, and background work live in a long-running daemon. The ratatui TUI and optional web API are thin clients that attach (socket/TCP/stdio). Closing the TUI must not stop background autonomy.

## Consequences

Requires a client-daemon protocol and slightly higher always-on resource use. Enables hooks/schedules while the UI is closed and cleanly separates orchestration from presentation. Early UX can still auto-start the daemon under `liberado tui`.

## Rejected alternatives

TUI-first ownership of the main agent loop (would force a rewrite for headless background work). Embedding the only long-running loop inside the UI process.

## Implementation and tests

- See crate Rustdoc and tests for the current implementation of this decision.

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: Background autonomy requires long-running processes. If the main agent loop lives inside the TUI process, adding real background work (hooks firing while TUI is closed) will require a significant rewrite.

**Current state in design**: ratatui TUI listed as "primary"; axum API as "optional." Not explicit about daemon ownership.

**Open questions**:
- Does the main agent loop run inside the TUI process, or is there a separate long-running daemon process that the TUI attaches to as a client?
- How do hooks and background work interact with a closed TUI?

**Recommended path**:
- **Daemon-first**. The core agent loop + liberado dispatcher + MCP/hook client lives in a long-running daemon (or the main-agent crate can run as a service).
- The ratatui TUI is a **thin client** that connects to the daemon (via local socket, Tailscale, or stdio).
- This cleanly supports background autonomy without forcing the TUI to stay open.
- Start simple: single binary that can run in "daemon mode" or "TUI-attached mode" for v1.

**Status**: Complete

Decision 2: Daemon-first vs. TUI-first Process Model (Finalized)
Decision: Daemon-first architecture.
The core agent loop, liberado dispatcher, MCP/hook clients, and background work ownership live in a long-running daemon process. The ratatui TUI and optional webserver (axum) are clients that attach to the daemon.
Rationale

True background autonomy (hooks firing on schedules or vault changes, scheduled reviews, reactive behaviors) requires a process that continues running even when the TUI is closed.
Putting the main agent loop inside the TUI process would force a significant refactor later when adding headless/background capabilities.
A daemon model cleanly separates core logic from user interfaces, improving maintainability and testability.
This aligns with the goal of low mental load: the user can close the TUI without losing ongoing work or scheduled behaviors.

Architecture Overview
text+--------------------------------------------------------------+
¦                     Liberado Daemon                          ¦
¦  (long-running process)                                      ¦
¦                                                              ¦
¦  • Main Agent Loop + ContextPolicy                           ¦
¦  • liberado-tool-helper-mcp (dispatcher)                     ¦
¦  • MCP client connections                                    ¦
¦  • Hook client / message handling                             ¦
¦  • Background trigger integration (vault emitter, timers)    ¦
¦  • Optional: lightweight webserver (axum) inside daemon      ¦
+--------------------------------------------------------------+
                               ¦
          +--------------------+--------------------+
          ?                    ?                    ?
   +--------------+     +--------------+     (optional)
   ¦   ratatui    ¦     ¦  Web / API   ¦
   ¦     TUI      ¦     ¦   Clients    ¦
   ¦  (client)    ¦     ¦  (Tailscale) ¦
   +--------------+     +--------------+
Key Design Points
1. Daemon Ownership

The daemon owns the main reasoning loop, ContextPolicy, and calls to liberado.
It manages connections to MCPs and receives messages from hooks.
Background autonomy (hooks, scheduled triggers, vault-change reactions) runs inside or is coordinated by the daemon.

2. TUI as a Client

The ratatui TUI is a separate binary that connects to a running daemon.
Connection options (in priority order for v1):
Unix domain socket (localhost, fast, simple)
TCP on localhost (easier cross-platform)
Stdio (for very simple early development)

The TUI can start the daemon automatically if one is not already running (common pattern).

3. Webserver / API

The optional axum webserver can either:
Run inside the daemon process (simpler for v1), or
Run as a separate lightweight client that proxies to the daemon.

Recommendation for v1: Run the webserver inside the daemon when enabled (via feature flag or config).

4. Lifecycle & Running Modes

Command Example,Behavior,Use Case
liberado daemon,Starts headless daemon,"Servers, always-on setups"
liberado tui,Attaches TUI to daemon (starts if needed),Daily interactive use
liberado (default),Starts TUI + daemon if needed,Simple daily driver


5. Communication Protocol

Use a simple request/response protocol over the chosen transport (Unix socket / TCP).
JSON-RPC 2.0 is a reasonable default (well-supported, easy to implement).
Messages should include:
User prompts / chat
Structured results from liberado / subagents
Status / health information
Capability / context requests (if needed)


6. Background Work & Detach

Hooks and scheduled behaviors continue running in the daemon even after the TUI disconnects.
The daemon should handle graceful shutdown and persistence of in-flight work where necessary (mostly via the vault).

Implications for Code Structure

main-agent crate ? becomes the core daemon logic (loop, ContextPolicy, liberado integration, MCP/hook handling).
tui crate/binary ? thin client that connects and renders the interface.
Clear separation between orchestration logic and presentation.
Easier to test the core agent behavior independently of the UI.

Trade-offs
Advantages:

Proper support for background autonomy.
Better separation of concerns.
TUI can be closed/reopened without interrupting work.
Easier to add other interfaces later (mobile, web, etc.).

Disadvantages:

Slightly more complex than a single-process TUI for very early development.
Requires defining a client-daemon communication protocol.
Slightly higher resource usage (daemon always running).

Mitigation: For early development we can make the daemon start automatically and transparently when running liberado tui, so the experience still feels simple.
