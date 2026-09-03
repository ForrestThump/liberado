# Liberado: Rust-Native Personal AI Life Operating System

**Version**: v0.3 (June 2026)  
**Status**: Historical vision document — **superseded as the cold-start reference** by
[`architecture/overview.md`](architecture/overview.md). This v0.3 draft predates the actual
crate layout: names like `liberado-dispatcher`, `liberado-memory-mcp`, and `liberado-tool-helper-mcp`
below are the *proposed* names, not what shipped (the real crates are `dispatcher`, `mcp`, etc. — see
`overview.md`'s crate table). Kept for the rationale and vision behind decisions that did land
(cross-referenced in [`architecture-decisions.md`](../decisions/README.md)); do
not treat the crate/module names or file layout below as current.  
**Owner**: Shiloh Mangus  
**Goal**: A minimal, security-first, token-efficient, fully auditable personal AI Life OS in Rust. It replaces heavier plugin-marketplace systems (e.g. OpenClaw) with curated, hand-audited components while enabling real background autonomy and loose coupling. Built directly on existing Turbovault + liberado-tool-helper-mcp work.

---

## Vision

Liberado is a personal Life OS that:

- Uses a structured **Obsidian Markdown vault** (via Turbovault) as the single source of truth for memory, tasks, calendar, decisions, goals, reviews, and knowledge.
- Keeps the **main agent thin** — focused on high-level reasoning, your current life context, and orchestration. It never receives massive tool schemas or full plugin lists.
- Uses **`liberado-dispatcher`** as an intelligent **goal-understanding dispatcher** (not just a surfacer). It understands the main agent's intent and decides the most efficient execution strategy, grounded in the procedural memory held by **`liberado-memory-mcp`** (the renamed `liberado-tool-helper-mcp`).
- Exposes capabilities through two complementary types of small, audited Rust components:
  - **MCPs** (Model Context Protocol servers): For doing work (tools and actions).
  - **Hooks** — thin receivers for background triggers and events.
- Enables **true background autonomy** — work happens on schedules or vault changes without the user initiating or thinking about it.
- Maintains **very loose coupling and modularity** — components can live in separate repos or a clean workspace; extensions are additive.
- Prioritizes **token efficiency** via high-signal context only + smart dispatching + summarized subagent reports.
- Stays **Rust-native, compiled, and auditable** with strong containment (path/zone guards, capability gates, secret isolation, hash-protected writes).
- Presents a **low mental-load interface** via two peer interaction modes, both suitable for daily use alongside a full-time job, family, ADHD management, and homelab: (1) a **ratatui TUI** for live conversation, and (2) an **async Obsidian inbox** — drop a note in `inbox/` from any device (Syncthing-synced), and the system processes/files it and reports back in the vault, no running conversation required (see `liberado-inbox-spec.md`).
- Is fully **provider-agnostic** — the scaffolding is custom; any inference provider (DeepSeek to start, others, or local models) can be used, including different models for main agent vs. subagents.

The system compounds over time: the same vault works for the human in Obsidian and for the agent. Background behaviors are added by creating or extending narrow hooks + pointing minimal external triggers at them.

---

## Core Principles

1. **Filesystem (vault) as the brain** — Structured Markdown + frontmatter and folder hierarchies (`calendar/2026/06/`, `tasks/`, `decisions/`, `goals/`, `reviews/`, `knowledge/`). Turbovault provides search, graph, frontmatter queries, and atomic edits. No mandatory vector DB for core memory.

2. **Thin main agent + intelligent dispatcher** — The main loop stays small. `liberado-dispatcher` understands goals and chooses execution strategy (direct MCP invoke for simple cases, or subagent dispatch for complex ones), backed by `liberado-memory-mcp` for learned guidance. This keeps main context clean and token usage low.

3. **MCPs for work, hooks for events** — Clear separation:
   - MCPs = curated, focused capabilities (tools/actions).
   - Hooks = thin event receivers that enable background autonomy.

4. **Loose coupling & modularity by design** — Hooks and MCPs are narrow. They can be developed, versioned, enabled/disabled, or replaced independently (separate repos or workspace crates). Shared concerns live in common libraries.

5. **Thin protocol layer for hooks (no integrated event systems)** — Hooks are lightweight HTTP webhook receivers. They do **not** contain cron, file watchers, or polling. Triggering comes from two sources: (a) **vault changes** via the daemon's single subscription to Turbovault's native change stream — the daemon does loop-breaking/attribution centrally (see `liberado-vault-concurrency-spec.md`) and routes already-attributed events to hooks; (b) **non-vault triggers** (systemd timers, git/docker/homelab hooks) that POST the standardized event payload directly to a hook webhook. There is **no hand-built `vault-change-emitter`** — that role is filled by Turbovault's subscription + the daemon's central attribution layer. This maximizes compatibility with existing hook systems and keeps hooks small.

6. **One hook per major event class/domain** — Group related events (e.g., all decision-related events in `decisions-hook`). Aim for 6–10 total hooks rather than 20 tiny ones or one monolith. Use a shared `liberado-hook-common` crate so individual binaries stay tiny and low-overhead.

7. **Capability containment & secret isolation everywhere** — Every MCP and hook enforces path/zone rules, capability gates, hash-protected writes, and ensures raw secrets/credentials never reach the LLM. Guards are implemented in Rust inside the components.

8. **High-signal context + token hygiene** — Explicit `ContextPolicy` in the main agent loads only goals/outcomes summaries, recent high-signal decisions, today's context, and liberado guidance. Vault details and subagent reports are fetched/summarized on demand.

9. **Provider-agnostic scaffolding** — Custom Rust agent loop (Rig preferred for speed on tool calling/memory policies, or thin custom tokio + reqwest). Easy to switch models/providers. Subagents can use different models.

10. **Low mental load & real-life fit** — ratatui TUI primary. Background work reduces routine load. System is maintainable alongside full-time work, family, and homelab. Everything local-first and auditable.

---

## High-Level Architecture

```
+-----------------------------------------------------------------------------+
¦                          Main Agent Loop (Thin)                             ¦
¦  - Rig (preferred) or thin custom tokio loop                                ¦
¦  - Explicit ContextPolicy (high-signal only)                                ¦
¦  - Calls liberado first for goal understanding + dispatch decision          ¦
¦  - Receives user chat + (optionally) hook messages / vault updates           ¦
¦  - Streams responses via ratatui TUI (primary) + optional axum API          ¦
+-----------------------------------------------------------------------------+
                                ¦
          +---------------------+---------------------+
          ?                     ?                     ?
+------------------+  +------------------+  +------------------------------+
¦  liberado        ¦  ¦  Curated MCPs    ¦  ¦  Thin hooks (per event class) ¦
¦  -dispatcher     ¦  ¦  (4–6 in v1)     ¦  ¦  (6–10 total, grouped)        ¦
¦                  ¦  ¦                  ¦  ¦                               ¦
¦  Intelligent     ¦  ¦  • tasks (hardened)¦  ¦  • decisions-hook             ¦
¦  Goal Dispatcher ¦  ¦  • calendar/rollup¦  ¦  • tasks-hook                 ¦
¦  + Tool Surfacer ¦  ¦  • decisions      ¦  ¦  • reviews-hook               ¦
¦                  ¦  ¦  • 1–2 more       ¦  ¦  • calendar-hook              ¦
¦  - Understands   ¦  ¦                  ¦  ¦  • ...                        ¦
¦    main goal     ¦  ¦  All with:       ¦  ¦  All are thin HTTP webhook    ¦
¦  - Simple ?      ¦  ¦  - Path/zone     ¦  ¦  receivers + reaction logic   ¦
¦    direct MCP    ¦  ¦    guards        ¦  ¦  (no cron/watcher inside)     ¦
¦    invoke +      ¦  ¦  - Hash writes   ¦  ¦                               ¦
¦    clean report  ¦  ¦  - Capability    ¦  ¦  Use shared                   ¦
¦  - Complex ?     ¦  ¦    gates         ¦  ¦  liberado-hook-common crate   ¦
¦    subagent      ¦  ¦  - Secret        ¦  ¦  for low overhead             ¦
¦    dispatch      ¦  ¦    isolation     ¦  ¦                              ¦
+------------------+  +------------------+  +------------------------------+
          ¦                     ¦                     ¦
          ¦                     ¦                     ¦ HTTP webhook (standard)
          ¦                     ¦                     ¦ (from external triggers)
          ?                     ?                     ?
   Obsidian Vault (Markdown + frontmatter + folders)     Trigger Sources
   calendar/2026/06/...                                   • Turbovault change stream
   tasks/                                                   ? daemon subscription
   decisions/                                               ? central attribution/de-loop
   goals/                                                 • systemd timers ? webhook
   reviews/ (LEARN-style outcomes & patterns)             • git/docker/homelab ? webhook
   knowledge/ (wikilinks + graph)
```

**Key flows**:
- User prompt ? Main agent ? liberado (goal understanding) ? direct MCP or subagent ? result/report back (summarized or vault artifact) ? main agent responds.
- Background event (timer or vault change) ? Trigger source ? hook webhook ? hook reaction (may use liberado dispatch or write structured output to vault) ? optional high-signal message to main agent or daily briefing.

---

## Component Details

### 1. Main Agent & ContextPolicy (Thin Orchestrator)

- **Implementation**: Rig (recommended starting point for tool calling, streaming, provider abstraction, and memory policy foundations) or minimal custom `tokio` + `reqwest`/`async-openai` loop for maximum control.
- **Core responsibility**: Maintain conversation, load high-signal context via `ContextPolicy`, call liberado for dispatch decisions, execute recommended actions, stream responses.
- **ContextPolicy** — a **deliberately dumb, inference-free life header** (full spec:
  `liberado-context-policy-spec.md`). The always-loaded context is tiny — **under two short
  paragraphs** — and everything else is pulled on demand. The architecture is built around this
  minimal "system prompt" as the steady state, not a stripped-down mode. Two jobs:
  - **Session-start header** (deterministic Turbovault queries + template): today + one-line rollup,
    active goals (titles + status, capped), recent high-signal decisions (capped), an inbox line, and
    an "ask to load more" availability pointer. Nothing else.
  - **Per-turn background surfacing**: completed Detached subagent Reports, hook outputs, and pending
    proposals — the inbound channel that re-enters background autonomy into the main loop.
  - **On-demand expansion**: read-a-known-thing ? a tiny curated read-only toolset (Turbovault
    `search` / `read_note`); figure-out-or-do-something ? the dispatcher. **Never auto-loaded**: full
    tool/MCP schemas, note bodies, history, raw subagent traces.
  - Because durable life-state lives in the vault and is re-injected each session, ContextPolicy is
    the safety net that makes `/new` and aggressive Layer-2 session management cheap and lossless.

The main agent loop listens for user input (TUI) and receives background results via the per-turn
surfacing path above (vault-mediated; Decision 9).

### 2. liberado-dispatcher + liberado-memory-mcp

**Naming note**: The original draft called this single component `liberado-tool-helper-mcp` and
spoke of "elevating" it into the dispatcher. We split it into two cleaner pieces:

- **`liberado-dispatcher`** — a **new** out-of-band routing agent (runs inside the daemon, with its
  own small/fast inference). This is the "intelligent dispatcher." It is the most important
  component in the work-execution path.
- **`liberado-memory-mcp`** — the existing `liberado-tool-helper-mcp`, renamed. It remains a thin
  mem0-backed MCP exposing two isolated stores: **general** memory (user facts/preferences/history)
  and **procedural** memory (`search_tool_guidance` / `save_tool_guidance` — learned "use tool X for
  task Y" directives). The dispatcher **consumes** it as a backend; it is not itself the dispatcher.

The dispatcher's job is to take a goal + minimal context from the main agent and choose exactly one
of four actions — **execute directly**, **dispatch a subagent**, **report back**, or **ask the main
agent a follow-up** — using procedural memory to ground the choice and recording outcomes back to
procedural memory to improve over time.

> The full decision policy — classification criteria, the structured `DispatchDecision` output,
> safe-by-default thresholds, deterministic guardrails, and the learning loop — is specified in
> **`liberado-dispatch-logic-spec.md`** (resolves the remaining detail of Decision 1).

In short: a powerful router that keeps main-agent context free of tool schemas and low-level traces,
far beyond a simple tool recommender, while preserving token efficiency.

### 3. MCPs — Curated Work / Capability Servers (4–6 in v1)

Small, hand-audited Rust binaries exposing capabilities via MCP (stdio or SSE).

**v1 starting set** (expand only on proven need):
- `tasks-mcp` (harden existing PR #19 with zone guards + hash checks).
- `calendar-rollup-mcp` or combined with tasks.
- `decisions-mcp` (logging + basic outcome tracking).
- 1–2 high-ROI external (e.g., secure notifications or read-only email with strict capability limits).

**Every MCP must implement**:
- Path/zone containment checks.
- Hash-protected writes.
- Capability gates (what this MCP is allowed to touch).
- Secret isolation (raw credentials never leave the MCP boundary; LLM sees only results or authorized operations).

MCPs are the "doers." They are narrow and composable.

### 4. Hooks — Thin Event Receivers (6–10 Total, Grouped by Class)

**Design rules** (critical):
- Hooks are **thin protocol layers only**. They contain **no integrated cron, file watcher, or polling logic**.
- They expose a **standard HTTP webhook endpoint** (`POST /webhook` with JSON payload) for maximum compatibility with existing hook systems (systemd, git, Docker, automation tools, etc.).
- One hook per **major event class/domain** (group related events). Target 6–10 total rather than 20 tiny processes or one monolith.
- Use a shared `liberado-hook-common` crate containing:
  - Webhook server skeleton (axum or lighter).
  - Auth/validation.
  - liberado client.
  - Vault helpers.
  - Guard helpers.
  - Common event types.
- Each hook binary is then very small: it registers its event types and implements only its domain-specific reaction logic.
- Reaction logic may:
  - Write structured output to the vault.
  - Use liberado to dispatch work or subagents.
  - Send a high-signal message toward the main agent (or let the next ContextPolicy load surface it).
  - Trigger other hooks (carefully).

**Example hooks** (adjust based on highest-ROI needs):
- `inbox-hook` — **async capture + ambient analysis** (see `liberado-inbox-spec.md`): resolves an intent tier from override flags (`#ready-now` / `#hold-off`) + location, settle-debounces Syncthing-synced notes (~15 min default; whole-vault ambient analysis runs as a nightly sweep), dispatches at the tier's intensity, moves inbox items to `processed/` with a breadcrumb. The thinnest hook — all judgment is the dispatcher's.
- `maintenance-hook` — **vault hygiene + git backstop** (see `liberado-vault-maintenance-and-git-spec.md`): scheduled Syncthing-conflict lossless-merge, broken-link repair, health checks. Relies on the git-backed vault (homelab-authoritative; `.git/` excluded from Syncthing) so in-vault fixes are recoverable.
- `decisions-hook`
- `tasks-hook`
- `reviews-hook`
- `calendar-hook`
- `family-schedule-hook` (with strict containment)

**Overhead**: With the shared common crate + grouping, 6–10 small Rust hooks have very low idle memory/CPU (well under 200 MB total on typical hardware). Most are idle the vast majority of the time.

### 5. Triggering Layer

Triggering has two paths. Hooks themselves stay thin — they never watch the filesystem or poll.

**(a) Vault-driven reactivity — Turbovault subscription + daemon attribution (no custom emitter).**
The earlier "hand-built `vault-change-emitter`" is **superseded**. Turbovault already owns the
filesystem watcher and exposes a native change subscription (`subscribe_vault_events` /
`fetch_vault_events`, with a monotonic `seq` / `since_seq` resume cursor). The **daemon** holds a
**single** subscription and performs loop-breaking and provenance attribution **once, centrally**
(consumer-side hash join against the Turbovault audit log — see `liberado-vault-concurrency-spec.md`),
then routes the resulting **already-attributed, already-de-looped** events to the relevant hook. This
keeps the per-consumer join cost out of the hooks and gives one place to reason about cascades.

**(b) Non-vault triggers — direct webhook POST.**
- **systemd timers** (or a single small scheduler binary): POST a standardized event to the relevant hook's webhook on schedule.
- **Other homelab sources** (git hooks, Docker events, scripts): POST directly to the appropriate hook webhook.

**Standardized event payload** (used by both paths; for vault events the daemon fills in the
attribution fields before routing):
```json
{
  "event_type": "DecisionLogged",
  "timestamp": "2026-06-21T15:42:00Z",
  "source": "turbovault-subscription",     // or "systemd-timer", "git-hook", ...
  "correlation_id": "review-2026-06-21",    // idempotency + loop-breaking key
  "provenance": { "source": "human", "zone": "decisions" },  // null for non-vault triggers
  "payload": {
    "path": "decisions/2026-06-21/example.md",
    "summary": "Short high-signal excerpt or metadata"
  }
}
```

This keeps hooks small while letting any hook-capable system trigger background behavior, and ensures
no hook ever reacts to a change one of our own agents produced.

### 6. Vault Layer (Turbovault + Your Plugins)

Unchanged core strength:
- Primary MCP for search (BM25), frontmatter queries, graph analysis, atomic batch edits, templates, health checks.
- Extend with calendar/rollup and decisions support as needed.
- Same files the user interacts with daily in Obsidian.

---

## Example Flows (Self-Contained)

**User prompt flow**:
1. User: "Help me review recent decisions and suggest related goals."
2. Main agent loads high-signal context via ContextPolicy.
3. Calls liberado with goal.
4. liberado decides: complex ? dispatches subagent with narrow context + allowed MCPs (decisions-mcp, goals-related).
5. Subagent works, writes structured report to `reviews/2026-06-21/decision-review.md`.
6. Report (or summary) returns to main agent ? clean response to user.

**Background autonomous flow (example)**:
1. New decision file written to `decisions/` (by the human in Obsidian).
2. Daemon's Turbovault subscription surfaces the change; daemon attributes it (hash join vs audit log ? provenance `source: human`, not one of our agents ? not suppressed) and routes a standardized event to `decisions-hook`.
3. `decisions-hook` receives the already-attributed event, validates, then uses liberado dispatch or direct logic to analyze patterns or suggest related goals.
4. Writes analysis to `reviews/` or appropriate location.
5. Optionally surfaces high-signal summary in next daily briefing or main agent context.

---

## Implementation Guidelines

**Crate / Binary Layout (Cargo workspace recommended)**:

> **Note**: this is the original planned layout (v0.3). The actual crate map is in
> [`reference/crate-map.md`](reference/crate-map.md).

```
liberado/
+-- Cargo.toml (workspace)
+-- crates/
¦   +-- common/                    # Shared types, guards, error handling
¦   +-- hook-common/               # liberado-hook-common (webhook skeleton, helpers)
¦   +-- main-agent/                # Thin orchestrator + ContextPolicy + TUI
¦   +-- liberado-dispatcher/       # Out-of-band routing agent (new component)
¦   +-- liberado-memory-mcp/       # Renamed liberado-tool-helper-mcp: mem0-backed
¦   ¦                              #   general + procedural memory; consumed BY the dispatcher
¦   +-- mcp-tasks/                 # Example hardened MCP
¦   +-- mcp-decisions/
¦   +-- hook-decisions/            # Example thin hook
¦   +-- hook-reviews/
¦   +-- tui/                       # ratatui interface
¦   # NOTE: no vault-emitter crate — vault reactivity is the daemon's Turbovault
¦   #        subscription + central attribution (see §5 and the concurrency spec).
```

**Low-overhead hook pattern**:
- Every hook depends on `hook-common`.
- Binary is ~mostly just `main()` that starts the webhook server and dispatches to domain handlers.
- Compile-time feature flags or simple config to enable/disable specific event types.

**Provider switching**:
- All LLM calls go through the custom scaffolding (Rig abstractions or your own provider enum).
- Subagent spawning includes model/provider selection.

**Containment enforcement**:
- Implement inside each MCP and hook (and in common guards crate).
- Never trust input — validate zones, capabilities, and secrets at the boundary.

---

## v1 Minimal Scope (Ship Usable Daily Value Quickly)

**In scope for first working version**:
- Thin main agent loop (Rig or custom) with explicit ContextPolicy.
- Enhanced liberado as goal-understanding dispatcher (simple invoke vs. subagent dispatch).
- 2–4 MCPs (tasks hardened with guards, decisions, calendar/rollup basics).
- 3–5 hooks using the thin HTTP webhook + shared common pattern (start with decisions-hook, tasks-hook, reviews-hook).
- Triggering layer: daemon subscription to Turbovault's change stream + central attribution; systemd timer example for non-vault triggers.
- ratatui TUI (chat + simple activity view).
- Git-backed vault + lightweight ISA-inspired templates in `goals/` and `decisions/` (success criteria, verification, outcomes).
- End-to-end test with realistic prompts and one background flow.
- All components enforce containment/secret isolation.

**Explicitly out of scope for v1**:
- Full 20 hooks or heavy event bus.
- Integrated cron inside hooks.
- Heavy RAG / vector store (Turbovault search first).
- Complex multi-agent orchestration beyond simple subagent dispatch.
- Mobile/web UIs or voice.

This v1 already delivers token-efficient reasoning, real background autonomy for high-value areas, strong security, and low mental load.

---

## Security, Containment & Token Efficiency

- **Containment**: Path/zone checks + capability gates in every MCP and hook. Hash-protected writes. Secret isolation boundary.
- **Auditability**: Everything is Rust source + git on vault. Small binaries are easier to review than large plugin systems.
- **Token efficiency**: High-signal ContextPolicy + liberado smart dispatch + summarized subagent reports + on-demand vault access only.
- **Privacy**: Local-first. No cloud for core memory or sensitive actions. Tailscale for any remote access.

---

## Resource & Operational Considerations

- **Hook overhead**: With grouping (6–10 total) + shared `liberado-hook-common` crate, total idle memory stays low (well under 200 MB). Most hooks are idle the vast majority of the time. Suitable for mini-PC homelab hardware.
- **Management**: Systemd templated units + simple TUI status command. Easy to start/stop/restart individual behaviors.
- **Extensibility**: Add a new background behavior by creating/extending a hook (or small group) + configuring a trigger source to call its webhook. Minimal impact on existing system.

---

## Next Steps (Implementation Roadmap)

> **For current implementation status, see `ARCHITECTURE.md` § "Current status".** Several
> steps below are realized: the workspace exists (step 1), the reactive pipeline with
> dispatcher+orchestrator+executor is end-to-end wired (steps 3, 6, 7), and tests are in place (step 9).

1. **Set up workspace** — Create Cargo workspace with `common`, `hook-common`, `main-agent`, `liberado-dispatcher` crates.
2. **Implement ContextPolicy** — Define the struct and loading logic (always high-signal + on-demand Turbovault).
3. **Enhance liberado** — Add goal-understanding + simple vs. subagent dispatch logic + clean report formatting.
4. **Build first MCPs** — Harden tasks with guards; add decisions basics.
5. **Build first hooks + common crate** — Thin HTTP webhook receiver pattern using shared library. Start with 2–3 (decisions, tasks, reviews).
6. **Wire the trigger layer** — daemon subscription to Turbovault's change stream + central attribution/de-loop that routes events to hooks; add a systemd timer example for non-vault triggers.
7. **Wire main agent loop** — Integrate liberado calls, ContextPolicy, and optional hook message handling (vault-mediated or direct).
8. **Add ratatui TUI skeleton** — Chat + tool/activity view.
9. **Test end-to-end** — Realistic user prompt + one background autonomous flow.
10. **Document & iterate** — Update this design doc from real usage. Add more hooks/MCPs only when daily value is proven.

---

## Summary for Implementers (No Prior Context Needed)

Read this document end-to-end. The system is:
- Thin main agent + powerful liberado dispatcher.
- MCPs for work (curated, guarded).
- Thin hooks (HTTP webhook receivers, grouped by event class, using shared common crate) for background triggers.
- Triggers: daemon's Turbovault subscription (vault changes, centrally attributed) + direct webhooks (timers, homelab hooks).
- Same Obsidian vault as source of truth.
- Rust-native, provider-agnostic, containment-enforced, token-efficient, loosely coupled, and designed for low overhead and real daily use.

Start with the workspace layout, ContextPolicy, enhanced liberado, and 2–3 hooks + triggers. Everything else follows from the principles above.

This design delivers background autonomy and modularity without the ceremony or lock-in of heavier systems, while staying maintainable alongside real life.

---

**Ready to build.** The document above is now the single source of truth for implementation. A fresh model can start coding from here and stay aligned with the vision. 

If you want any section expanded (e.g., exact ContextPolicy struct sketch, minimal hook binary example code, event payload schema, or crate `Cargo.toml` examples), just say the word and I'll add it or create companion files. We're in a great place to start shipping the first pieces.
