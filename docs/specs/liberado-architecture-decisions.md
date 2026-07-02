# Liberado Architecture Decisions Log

**Purpose**: Consolidated, prioritized list of key architectural and design decisions. Grouped by importance so we can resolve them in sequence before writing code that would be expensive to change.  
**Status**: Living document. Each decision includes current state, open questions, and a recommended path based on our principles (loose coupling, security-first containment, token efficiency via context partitioning, low overhead, maintainability alongside real life, provider-agnostic scaffolding).  
**Last Updated**: 2026-06-26

---

## Tier 1: Load-Bearing Decisions (Most Expensive to Reverse)

These decisions thread through the entire system architecture, security model, process layout, and token efficiency claims. Resolve these first.

### 1. Liberado Invocation Model + Inference Responsibility
**Why it matters**: This is the central architectural question. It determines whether liberado is a simple tool, an out-of-band orchestrator, or a full agent with its own inference. It directly affects token accounting, latency, framework fit (Rig vs custom loop), and how we realize quadratic prefill savings.

**Current state in design**: Treated as an "intelligent goal-understanding dispatcher" that can do direct MCP invocation or subagent dispatch. Not fully specified whether it runs its own LLM call.

**Open questions**:
- Is liberado invoked as a normal tool call inside the main agent's tool-calling loop, or does the main loop intercept intent out-of-band and hand it to liberado?
- Does liberado perform its own inference (LLM call) to classify simple vs. complex goals and choose strategy?
- How does liberado receive the necessary context without duplicating large parts of the main agent's context?

**Recommended path**:
- Make liberado a **separate, narrowly-scoped agent** (with its own lightweight context policy) that the main loop calls out-of-band when needed.
- It performs a small, focused inference step (goal classification + dispatch strategy) using a fast/cheaper model when possible.
- Optimize for **disjoint context partitions** to realize real quadratic savings (dispatcher sees goal + filtered tool catalog; subagent sees goal + chosen schemas + work context; minimal overlap).
- This is more powerful than a pure tool call and justifies the extra hop for local inference and long-context regimes.

**Status**: Complete.

Decision 1:
Liberado operates as an out-of-band intelligent dispatcher agent. It has access to the full MCP catalog (names + short descriptions) and receives minimal, goal-specific context from the main agent. It can:

Directly execute simple, high-confidence tool calls,
Spawn narrowly-scoped subagents with disjoint context, or
Escalate back to the main agent with structured uncertainty signals when clarification or higher-level judgment is needed.

The main agent context remains protected from tool definitions, internal dispatch reasoning, and low-level tool execution traces.

**Routing detail resolved in `liberado-dispatch-logic-spec.md`**: the dispatcher chooses among four terminal actions — `ExecuteDirect`, `DispatchSubagent`, `Clarify` (to the main agent), and `Report` (the return type of the first two). Choice is made by a 5-step pipeline (retrieve procedural guidance → classify via small inference → downgrade-only deterministic guards → act → record outcome). Correctness is engineered, not assumed: routing is **safe-by-default** (uncertainty degrades toward Clarify/proposal, never toward an irreversible action), guards can only *downgrade* risk (capability/zone-write-class/consequence/reaction-depth/confidence), and the decision is a typed, traced, eval-tested artifact (Decisions 12, 16). The component split (new `liberado-dispatcher` consuming the renamed `liberado-memory-mcp` for general + procedural memory) is recorded in `life-os-architecture.md` §2.

### 2. Daemon-First vs. TUI-First Process Model
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
text┌──────────────────────────────────────────────────────────────┐
│                     Liberado Daemon                          │
│  (long-running process)                                      │
│                                                              │
│  • Main Agent Loop + ContextPolicy                           │
│  • liberado-tool-helper-mcp (dispatcher)                     │
│  • MCP client connections                                    │
│  • Hook client / message handling                             │
│  • Background trigger integration (vault emitter, timers)    │
│  • Optional: lightweight webserver (axum) inside daemon      │
└──────────────────────────────┬───────────────────────────────┘
                               │
          ┌────────────────────┼────────────────────┐
          ▼                    ▼                    ▼
   ┌──────────────┐     ┌──────────────┐     (optional)
   │   ratatui    │     │  Web / API   │
   │     TUI      │     │   Clients    │
   │  (client)    │     │  (Tailscale) │
   └──────────────┘     └──────────────┘
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

main-agent crate → becomes the core daemon logic (loop, ContextPolicy, liberado integration, MCP/hook handling).
tui crate/binary → thin client that connects and renders the interface.
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

### 3. MCP Transport and Process Model (Multiple Consumers)
**Why it matters**: The main agent (via liberado), subagents, and hooks may all need to invoke the same MCPs (e.g., tasks-mcp). Stdio is simple but couples lifecycle and makes sharing difficult.

**Current state in design**: "stdio or SSE" left open. Multiple consumers not yet addressed.

**Open questions**:
- Do MCPs run as long-lived services (HTTP/SSE) or are they spawned per-caller (stdio)?
- Are MCPs shared singletons or per-caller instances?
- How does this interact with capability filtering per caller (main agent vs subagent vs hook)?

**Recommended path**:
- Prefer **long-running HTTP/SSE MCP services** for v1 and beyond (easier sharing, connection model, and capability enforcement at the boundary).
- Use stdio only for very simple/one-off MCPs if needed.
- Design MCPs to be **stateless or narrowly stateful** so multiple callers can use them safely.
- Capability filtering happens at dispatch time (liberado or hook) before calling the MCP.

**Status**: Complete

Decision 3: MCP Transport and Process Model (Finalized)
Decision:
Support both HTTP/SSE and stdio transports, with a strong preference for long-running HTTP/SSE MCP services. Stateless MCPs are preferred. Stateful MCPs are allowed when necessary, but must use narrow resource-level locking rather than broad MCP-level locks.
Rationale
Multiple consumers (main agent via liberado, subagents, and hooks) will eventually need to interact with MCPs concurrently. A pure stdio model creates lifecycle and sharing problems in this scenario. Long-running HTTP/SSE services make concurrent access more natural while still allowing capability narrowing.
Stateless (or narrowly stateful) MCPs are dramatically easier to reason about, test, and scale. However, some capabilities genuinely require state (e.g., sessionful connections or complex in-memory coordination), so we should not ban stateful MCPs outright.
When state is required, broad locks on the entire MCP would severely limit concurrency. Narrow locking at the resource or zone level is a better fit with the capability-based model developed in Decision 4.
Key Points

Primary transport: Long-running HTTP/SSE (or WebSocket) MCP services. This is the recommended model for most MCPs.
Secondary transport: stdio — fully supported, particularly useful for simple or early-stage MCPs.
Stateless preferred: Most MCPs should be designed to be stateless or use optimistic concurrency where possible.
Stateful MCPs: Allowed, but should use narrow resource-level locking (e.g., per Zone, per resource ID, or per specific object) instead of locking the entire MCP.
Concurrency support: The architecture should enable multiple subagents (and the main agent) to call MCPs concurrently when their capabilities and locked resources do not conflict.
Documentation requirement: Every MCP should clearly declare whether it is stateless or stateful and describe its concurrency/locking behavior.

Interaction with Other Decisions

Daemon-first (Decision 2): Long-running HTTP MCP services integrate naturally with the daemon model.
Capability narrowing (Decision 4): Narrow resource locking pairs well with dynamic capability narrowing during dispatch. Liberado can grant reduced capabilities and reduced locking scope to subagents.
Future multi-agent work: This model supports the goal of allowing multiple agents/subagents to operate concurrently without excessive contention.

Trade-offs
Advantages:

Good support for concurrent subagent execution.
Cleaner sharing of MCPs across different callers.
More scalable and future-proof than a pure stdio model.
Narrow locking preserves concurrency better than coarse-grained locks.

Disadvantages:

HTTP/SSE services are slightly more complex to implement than simple stdio MCPs.
Stateful MCPs still require careful design around locking and recovery.

Final Recommendation
Adopt HTTP/SSE as the primary transport with support for stdio. Prefer stateless MCPs, but allow stateful ones when genuinely needed — using narrow resource-level locking rather than broad MCP locks. This provides the best balance between simplicity, concurrency, and future multi-agent support.

### 4. Capability / Zone Model — Concrete Data Structures and Semantics
**Why it matters**: This is the foundation of the entire security and containment story. "Path/zone guards" and "capability gates" are mentioned throughout but never defined. Retrofits here are extremely expensive.

**Current state in design**: Mentioned repeatedly but underspecified. No concrete types yet.

**Open questions**:
- What exactly is a "Zone"? (Path glob set? Named region of the vault? Hierarchical?)
- What is the capability grammar? (e.g., `Read(tasks/*)`, `Write(decisions/)`, `Invoke(tasks-mcp:complete)`)
- Who is the authority that grants capabilities? (Static per-component config? Dynamic from liberado at dispatch time? Vault-based policy?)
- How are capabilities passed and checked at every boundary (MCP, hook, subagent spawn)?

**Recommended path**:
- Define concrete types in a shared `common` crate **before writing any MCP or hook code**:
  - `Zone` (simple path prefix + optional glob for v1)
  - `Capability` (enum or structured type)
  - `CapabilitySet` / `Policy`
  - `check_capability(subject, action, target)` function
- Start with **static per-component + dispatch-time grants** from liberado.
- Make every MCP and hook call the guard on entry.
- This single artifact unblocks a huge amount of the security model.

**Status**: Complete

Defined as `liberado-permissions-idea.md`. The enforcement boundary is at each MCP / hook. Permission can be narrowed at dispatch, but never expanded. Simple yaml defition of permissions. Zones for areas of permission.

### 5. Vault Concurrency, Write Provenance, and Loop-Breaking
**Why it matters**: The vault is the shared database with many writers (human in Obsidian, main agent, subagents, hooks). Without clear rules, we risk write races, data loss, and infinite reaction loops.

**Current state in design**: "Hash-protected writes" mentioned. Vault-emitter is responsible for change detection but its full responsibilities are underspecified.

**Open questions**:
- Which paths are human-only vs. agent-writable?
- How do we handle concurrent edits (Obsidian + agent)?
- How do we prevent reaction loops (agent writes → emitter fires hook → hook writes → emitter fires again)?
- What provenance tagging is required on agent-originated changes?

**Recommended path**:
- Make the **vault-emitter** a first-class, well-specified component (not "thin").
- Require **provenance tagging** on all agent writes (e.g., frontmatter field `agent_write: true` + correlation ID).
- Build **debouncing + loop detection** into the emitter from day one.
- Define clear human-vs-agent write boundaries per zone.
- Use hash-protected writes + optimistic concurrency where possible.
- Document this explicitly — it is load-bearing for reliable background behavior.

**Status**: Complete

Decision 5: Resolved in `liberado-vault-concurrency-spec.md`. Summary:
- **Provenance lives on the Turbovault audit log, not frontmatter** (frontmatter is last-writer-only state and goes stale on direct Obsidian edits). Rides on `AuditEntry.metadata._liberado_provenance` today; migrates to a typed field if the upstream proposal lands. `source` + `correlation_id` are mandatory on every agent write.
- **Loop-breaking via Approach A (consumer-side hash join)**: attribute an observed change by matching `sha256(nfc(content))` against the `after_hash` of the latest audit entry for that path. Match + non-human + recent → suppress; no match → external/human edit → react. Robust to races, coalescing, and human-edits-after-agent. A bounded seen-correlation set + child correlation IDs break cross-hook A→B→A chains; `MAX_REACTION_DEPTH` halts cascades.
- **Consume Turbovault's native subscription (PR #24), not a custom emitter.** The daemon holds one subscription and does the hash-join + de-loop **centrally**, then routes already-attributed events to thin hooks. This supersedes the hand-built `vault-change-emitter` in `life-os-architecture.md` §5 (non-vault triggers still POST webhooks directly).
- **Concurrency stays optimistic** with the structured `ConcurrentModification { path, expected, actual }` error; agents re-read and retry (bounded) rather than overwrite.
- **Per-zone write classes** (`human_only` / `agent_writable` / `proposal_only` / `shared`) enforced at the MCP/hook boundary; unlisted zones default to `proposal_only` (fail safe).
- **Idempotency**: correlation ID is the idempotency key; vault-as-journal (pending→working→done markers) makes redelivery safe.
- **Attribution is best-effort, never a security boundary** (`None` = treat as external). Security stays with the Decision 4 capability/zone model.
- No upstream merge blocks the architecture — every upstream dependency has a working fallback behind a thin adapter (see spec §8.1).

---

## Tier 2: High-Impact Seams (Decide Before Building Relevant Components)

### 6. Event Delivery Semantics, Idempotency, and Durability
**Why it matters**: Bare HTTP POST is at-most-once. Background autonomy that silently drops work on restart is not acceptable.

**Recommended path**:
- Design hook reaction handlers to be **idempotent** from the start (use correlation IDs + check-if-already-processed).
- Use the **vault itself as the durable journal** where possible (write intended work as a pending item, then process).
- For higher reliability later, consider a small durable queue, but keep v1 vault-centric.

**Status**: Complete (specified in `liberado-vault-concurrency-spec.md` §7).

Decision 6: Both delivery paths (Turbovault subscription with drop-and-resync; webhook POST) are **at-most-once**, so reaction handlers are **idempotent by construction**. The `correlation_id` carried on every standardized event is the **idempotency key**. Before acting, a hook checks a durable journal marker (`.liberado/reactions/<correlation_id>.json`: pending → working → done) — redelivery re-enters at the existing marker instead of double-acting. **Vault-as-journal** is the v1 durability story; no separate durable queue. On subscription **drop/overflow**, the contract is *resync from authoritative state* (bounded re-scan of the hook's zone), never "assume we saw every event." **Event ordering is not guaranteed** — handlers must converge regardless of order (idempotent, not order-dependent). A durable queue is deferred until vault-centric journaling proves insufficient.

### 7. Monorepo vs. Separate Repos Strategy
**Why it matters**: "Loose coupling via separate repos" conflicts with heavy use of shared crates (`hook-common`, guards, types).

**Recommended path**:
- Commit to a **Cargo workspace (monorepo)** for v1–v2.
- Design crate boundaries cleanly so extraction to separate repos later is low-friction if needed.
- Drop the "separate repos from day one" aspiration for now; it creates version skew and complexity without enough benefit yet.

**Status**: Complete

Decision 7: **One Cargo workspace (monorepo)** for the Liberado system — `common`, `hook-common`, `main-agent` (daemon), `liberado-dispatcher`, `liberado-memory-mcp`, the MCP crates, the hook crates, and `tui`. Crate boundaries are kept clean so any crate can be extracted to its own repo later with low friction. **External dependencies** (`turbovault`, `turbomcp` and its crates) are *not* vendored into the workspace — they are consumed as **path dependencies during co-development** (the repos are checked out as siblings and Shiloh actively contributes to both) and **pinned to crates.io versions for release builds**. The existing `liberado-tool-helper-mcp` repo is folded in as the `liberado-memory-mcp` crate at implementation time. This resolves the original "loose coupling via separate repos vs. shared crates" tension in favor of shared crates now, extraction later if ever needed.

### 8. Subagent Execution Model (Isolation Level)
**Why it matters**: Affects security isolation, complexity, resource usage, and KV-cache pressure on local inference.

**Recommended path**:
- Start with **in-process subagents with strong capability filtering** for v1 simplicity.
- Design the interface so heavier subagents can later move to separate processes without changing dispatch logic.
- Optimize context slices for disjointness to control KV-cache memory and realize quadratic savings.

**Status**: Complete (interfaces in `liberado-dispatch-logic-spec.md` §4, §10).

Decision 8: **In-process subagents** (tokio tasks in the daemon) for v1, capped at `MAX_CONCURRENT_SUBAGENTS` (default 2) for KV-cache/homelab bounds. They are spawned through a `Subagent` boundary that takes `(goal, CapabilitySet, allowed_mcps, success_criteria, model, correlation_id)` and returns a `Report` — **the dispatcher never knows whether a subagent ran in-process or out-of-process**, so moving heavy/experimental subagents to separate processes later requires no dispatch-logic change. **Isolation model, stated honestly**: in-process subagents share the daemon's memory space, so their *only* containment is **capability narrowing enforced at the MCP boundary** (no ambient authority — a subagent holds only a narrowed MCP client) plus secret isolation (raw secrets never reach any subagent; inference via the daemon). This is "trust-the-hand-audited-code" isolation, adequate for v1 because all subagent code and prompts are ours; it is **not** adversarial isolation. Out-of-process subagents (OS sandbox) are the upgrade path if/when subagents ever run less-trusted prompts. Context slices are kept disjoint (goal + narrowed schemas + work context only) for KV-cache control and the quadratic-prefill savings. **Isolation level is configurable** (`subagent.isolation = in_process | out_of_process`, default `in_process`) in the single-source config (Decision 14), so scaling to process isolation is a config change, not a source edit.

### 9. How Hook Messages Reach the Main Agent
**Why it matters**: Affects coupling between hooks and the core loop.

**Recommended path**:
- Primary path: **vault-mediated** (hooks write structured artifacts/summaries; ContextPolicy surfaces relevant items). Maximum loose coupling.
- Allow optional direct channel for high-priority cases later.

**Status**: Complete (surfacing mechanism in `liberado-context-policy-spec.md` §2 Job B).

Decision 9: **Vault-mediated only** for v1. Hooks and detached subagents **write structured artifacts** (with provenance + `correlation_id`) to agent-writable surfacing zones (`reviews/`, `proposals/`, hook output locations); they do **not** push into the daemon or know anything about the main loop. ContextPolicy's **per-turn Job B** surfaces unseen items (queried by a since-last-seen cursor / `surfaced: false` frontmatter, marked surfaced after showing). This keeps hooks maximally decoupled — a hook's only outbound contract is "write a vault artifact." A direct high-priority push channel is **deferred** until a real need (e.g. an urgent interrupt that can't wait for the next turn) is proven.

### 10. Secrets Backend and Inter-Component Auth
**Why it matters**: Critical for any MCP or hook that touches credentials (email, finance, notifications).

**Recommended path**:
- Use environment variables + systemd credentials for secrets in v1.
- Webhook auth: Shared secret header + Tailscale-only listening (or mTLS later).
- Never pass raw secrets through liberado or main agent context.

**Status**: Complete

Decision 10: Layered, leveraging what turbomcp already provides (API-key/JWT auth, secret zeroization, SSRF/path-traversal guards in `turbomcp-server`/`turbomcp-proxy`):
- **Secrets at rest**: environment variables + **systemd credentials** (`LoadCredential=`) for v1. Each MCP/hook process receives only the secrets it needs, injected at the process boundary.
- **Secret isolation (IronClaw pattern, per `liberado-permissions-idea.md`)**: raw secrets **never enter LLM context** — they are injected at the MCP boundary for the specific authorized operation only. The model sees results, not credentials.
- **Provider/inference keys live only in the daemon.** The main agent, dispatcher, and subagents run inference through the daemon's provider abstraction. MCPs/hooks that need reasoning use **MCP sampling** (`turbomcp-client`) so they never hold provider keys.
- **Inter-component auth**: local MCPs are reached over **Unix domain sockets** (filesystem permissions are the boundary; no network, no token needed). **Hook webhooks** (which accept input from external triggers) require a **shared-secret bearer header** and bind **Tailscale/localhost only**. Start with API-key/shared-secret; **JWT or mTLS** is the documented upgrade for any component that ever becomes network-exposed.

### 11. Human-in-the-Loop / Proposal & Approval Boundary
**Why it matters**: Some background actions (especially involving family, schedule, or external communication) should not be fully autonomous.

**Recommended path**:
- Define a clear **"proposal" output type** early.
- High-consequence actions emit proposals into a review location in the vault (or a dedicated inbox) rather than acting directly.
- Start conservative: most hook reactions write proposals or structured notes; only low-risk actions execute directly.

**Status**: Complete

Decision 11: A **Proposal** is a structured vault artifact — the typed output already referenced by the dispatch guards (`liberado-dispatch-logic-spec.md` §6) and concurrency write-classes (`liberado-vault-concurrency-spec.md` §3).

- **Shape**: a note in `proposals/` with frontmatter `{ id, correlation_id, source (agent/hook name), proposed_action (structured), rationale, status: pending|approved|rejected|expired, created, expires }` and a human-readable body.
- **What requires one** (computed, not classifier-judged): any write to a `proposal_only` zone, any high-consequence action (external comms, irreversible deletes, anything touching `Sensitive`/`FamilyShared`), and any guard-forced downgrade. Unlisted zones default to `proposal_only` (fail safe), so the **conservative default is "propose, don't act."**
- **Approval lifecycle (closes through the same machinery)**: agent writes proposal → ContextPolicy Job B surfaces it → user approves via the **TUI command *or* by editing `status: approved`** (so approval also works directly from Obsidian) → the approval is a human-sourced vault write that the daemon's subscription picks up → the daemon executes the `proposed_action` (now authorized) with the **proposal's `correlation_id`**, marks the proposal `done`, and links the resulting artifact. The execution write is agent-sourced and de-looped normally (concurrency spec §6). Expired/rejected proposals are never executed.
- **v1 conservative posture**: most hook reactions emit proposals or plain structured notes; only explicitly low-risk, `shared`/`agent_writable` actions execute directly.

**Status update (emit AND approve→execute landed, June 24, 2026)**: the full propose→approve→execute loop is closed. The EMIT path is wired — high-consequence *concrete* actions (an `ExecuteDirect` with a non-empty seed call list whose MCP is `External`/`Irreversible`) downgrade through `DispatchAction::Propose` → `Disposition::Propose(Proposal)` → a `proposals/<id>.md` artifact. The daemon writes it with **agent provenance**, so attribution suppresses the write (no self-reaction). On the APPROVE→EXECUTE side, a human's `status: approved` edit is picked up by the daemon's watch loop: `react()` checks for the `proposals/` path prefix before dispatching, routes to `handle_proposal_change`, which parses the frontmatter, validates it is Approved + non-expired + non-terminal, then calls `orchestrator.execute_approved()` with the proposal's `correlation_id` as provenance. Execution runs the approved `ToolCalls` directly against a runtime scoped to their MCPs (no classifier, no guards — the human edit is the authorization). On success the daemon flips `status` to `done` with agent provenance (loop-broken). Idempotency: terminal proposals (Done/Rejected/Expired) and non-actionable proposals (Pending) are left alone; infra errors from execution propagate (not marked done, retriable on the next watch cycle). Fuzzier high-consequence cases (empty-seed `ExecuteDirect`, `DispatchSubagent`, the magnitude-gate goal signal) still downgrade to `Clarify` for now.

### 12. Runtime Audit / Tracing Substrate
**Why it matters**: "Fully auditable" currently only covers code + git state. Runtime behavior (dispatch decisions, tool calls, hook reactions) is invisible.

**Recommended path**:
- Use `tracing` with structured spans from the beginning.
- Consider a durable append-only sink (even a simple file or vault-based log) for early usage data.
- Instrument liberado dispatch decisions especially — this data tells us whether the quadratic savings and dispatch logic are working.

**Status**: Complete

Decision 12: **`tracing` with structured spans** across the daemon, dispatcher, MCPs, and hooks from day one. **Dispatch decisions are instrumented specifically** (goal hash, retrieved guidance ids, action, confidence, rationale, guard downgrades, await/detach, outcome — `liberado-dispatch-logic-spec.md` §9) — this is the data that validates the routing and quadratic-savings theses.

**Two distinct trails, deliberately not conflated**:
1. **Turbovault audit log** (`turbovault-audit`, already exists): vault **write provenance** — before/after hashes + provenance metadata. Powers loop-breaking (Decision 5). A property of *vault writes*.
2. **Liberado runtime trace** (new): dispatch decisions, tool calls, hook reactions, errors. A property of *system behavior*.

**Sink**: the runtime trace is **append-only JSONL outside the vault markdown** (a daemon trace dir / gitignored `.liberado/trace/`), **never** into vault notes — high-volume trace writes would pollute the very change stream the system reacts to (the same lesson that put provenance on the audit log, not frontmatter). A richer sink (e.g. structured DB) is a later, non-blocking upgrade.

---

## Tier 3: Important but More Contained Decisions

### 13. Provider Capability Floor / Minimum Contract
Define the minimum tool-calling, JSON mode, and structured output reliability a provider must support so liberado's dispatch protocol doesn't break when switching models.

**Status**: Complete

Decision 13: **Role-tiered, not a single floor.**
- **Hard floor (every role)**: native **tool-calling** OR a reliable **JSON mode**. Text-only models are out of scope for v1 (constrained-decoding shim is the deferred escape hatch).
- **Control plane (main agent + dispatcher)** — the capable models. The **dispatcher's hard requirement is reliable structured output** (the typed `DispatchDecision`); the main agent needs solid tool-calling + instruction-following + conversational quality.
- **Work plane (subagents)** — floor is tool-calling; the **dispatcher picks the model per-dispatch by task complexity** (cheap ~8B for easy tasks, larger for hard ones — `DispatchDecision` already carries `model: Option<ModelChoice>`). This is where cheap models earn their keep.
- **Mechanism (feeds the config validator, Decision 14)**: a `ModelProfile` declares each model's capabilities (`tool_calling`, `structured_output`, `context_window`, tier/cost). Config assigns models→roles; the loader **fail-fast rejects** any model that doesn't meet its role's required caps (this is what keeps dispatch from breaking on a model swap). Optional startup **canary smoke-test** verifies tool-calling/JSON actually work.
- **Runtime resilience**: malformed structured output → treated like low confidence → bounded retry/repair (re-prompt with schema) → escalate to a stricter model or `Clarify`; never crash. Dispatcher runs at **temperature 0**. DeepSeek (starting provider) meets the control-plane bar.

### 14. Single Source of Truth for Config / Topology
Ports, socket paths, webhook URLs, subscription routing, capability grants, and all per-spec tunables.

**Status**: Complete (specified in `liberado-config-spec.md`).

Decision 14: **Single source of truth = one resolved, validated *model*, not one file.** Many small files (split by concern) are merged into one typed config object at startup. Key points:
- **Three concerns, owned distinctly**: `topology.toml` (wiring — components/ports/sockets/models), `policy.toml` (the central, auditable **security surface** — zones, write-classes, capability grants, secret references), and an optional `tuning.toml` (benign behavior overrides). Each setting is owned by exactly one place (validator rejects duplicate ownership).
- **Defaults live in code; config holds only deltas** — every tunable has a `Default` matching its home spec, so the config file can be **small or absent** and the system still works.
- **Out of the vault, homelab-local** (ssh in to edit); **agents never write config** (user-approval-gated config-through-the-system is a v2+ item). **Secrets are not config** (env/systemd by reference — Decision 10).
- **Fail-fast**: merge precedence is defaults → files → env (`LIBERADO_*`) → CLI; the merged whole is **cross-validated before the daemon serves anything** (unknown zones, missing MCPs, port collisions, dangling secret refs, triggerless hooks, etc.), surfaced on startup and via a `liberado config check` command. Conflicts are a load-time error, never a runtime surprise.

### 15. Frontmatter Schema Validation + Migration
Decide validation approach and migration strategy for frontmatter fields before the vault grows large.

**Status**: Complete

Decision 15: **Open, per-zone schemas, tiered by writer** — designed so schema never fights zero-friction capture.

- **Open schemas**: validation checks that *required* keys are present and well-typed; extra keys are always allowed (ad-hoc Dataview/Bases fields stay free). Structural, not closed.
- **Enforce on agent writes; normalize human writes, never block them.** Agent-created/processed notes must satisfy the zone schema (cheap for the agent, and satisfied by construction when it uses Turbovault `create_from_template` — **templates are the schema's concrete form**). Human-written notes — especially `inbox/` capture — have **no required frontmatter**; the system **backfills** schema keys when it next processes/files the note. Humans never hit friction.
- **Universal baseline** (on agent writes): `type` (task | decision | goal | review | proposal | knowledge | …) and `created`. `type` is the highest-value key — it drives Dataview/Bases queries, ContextPolicy lookups (`type=goal AND status=active`), and dispatcher routing. **Per-zone adds**: `status` for anything with a lifecycle; `proposals/` uses the full schema from Decision 11; `goals/`/`decisions/` carry the ISA-style success-criteria/outcome fields.
- **Explicitly NOT in frontmatter**: provenance / edit history — Turbovault's audit log already owns per-write events. Frontmatter holds current *state* only (same reasoning as the concurrency spec).
- **Validation** happens at the MCP write boundary (alongside capability checks): agent write violating schema → reject-and-retry; human write → accept + normalize lazily.
- **Migration**: optional `schema_version` makes migrations idempotent. **Lazy by default** (normalize on next write/process), with an **on-demand/maintenance batch migration** using Turbovault `inspect_frontmatter` / `query_frontmatter_sql` to find stale notes and `batch_execute` to update; the git backstop (maintenance spec) makes big-bang migration safe if ever wanted.
- Schemas are **declared in config** (`policy`/schema section — Decision 14), one authoritative definition.

### 16. Testing Seams for Nondeterministic Dispatch
Create a mocked-provider / recorded-fixture harness early so liberado's classification and dispatch logic can be tested deterministically.

**Status**: Complete (specified in `liberado-testing-and-eval-spec.md`).

Decision 16: **Integration tests injected at the two ingress points** — a simulated **user prompt** or a simulated **vault event** — run through the live pipeline with externals mocked (mock provider behind the provider trait, mock MCP servers, a real temp vault, injected clock + correlation-ID source), asserting on observable outcomes (vault writes, proposals, the `Report`, which tool calls fired or were suppressed). The key enabler: safety lives in **deterministic guards that run *after* the model**, so most of the system is exactly assertable and only classification *quality* (never safety) is probabilistic. Two verification methods inside scenarios: **mock-provider replay** (deterministic CI regression) and a **real-model eval suite** reporting routing accuracy + safe-default rate + a **safety-regression metric that must never increase**. **Logging is the fixture pipeline**: the Decision 12 trace → `record` mode → golden scenario → permanent regression test.

### 17. Conversation History Store
**Why it matters**: The chat agent (`main-agent`) holds conversation history in memory only — it is
lost on restart and exists as a single session. How we persist it is load-bearing *not* for v1 chat
but for everything the vision wants next: conversation **branching**, **parallel subagent dispatch**,
**fan-out conversations**, **debate** systems, and user **interruption**. Pick the wrong storage
*shape* now (a flat list, random ids, a mandatory DB daemon) and those become rewrites; pick the
right *seams* and they become additive.

**Open questions**:
- Does conversation history live in the vault (Pillar 1) or outside it?
- Linear list or a branchable structure on disk?
- What engine — JSON/JSONL, SQLite, Postgres, DuckDB — and does "future-proof for concurrent users"
  force a networked DB?
- How is it searched (grep vs FTS vs vectors)?

**Status**: Complete (full design in `liberado-conversation-store-spec.md`).

Decision 17: **An append-only log of message *nodes*, JSONL outside the vault, behind a
`ConversationStore` trait.** Key points:
- **Operational data, not vault knowledge.** Conversation history is the **same category as the
  Decision 12 runtime trace** — append-only JSONL *outside* the vault Markdown, for the identical
  reason: high-volume chat writes would pollute the change-stream the daemon reacts to. Pillar 1
  ("vault is source of truth") is about *knowledge*; it is not a claim that chat logs are notes. The
  vault bridge is a **one-way derived Markdown export** (a view, git-tracked, human/vector-friendly),
  never the system of record and never on the live write path.

  > **Clarifying note (2026-06-26 — do not rewrite history):** the matured pillars demote
  > "the vault is the source of truth." **The vault (TurboVault) is now the default, privileged
  > perception+storage plugin, not a hard dependency.** The core (dispatch / execute / MCP runtime /
  > chat / conversation-store) is vault-agnostic; the vault's coupling is isolated to the reactive
  > subsystem (watch + provenance loop-breaking), which becomes the vault plugin behind an
  > event-source trait (Decisions 18, 19). See the three pillars in
  > [`docs/architecture/overview.md`](../architecture/overview.md) and
  > [`docs/architecture/positioning.md`](../architecture/positioning.md). Wherever an earlier
  > decision in this log says "the vault is the source of truth," read it through this clarification.
- **One log, everything else is a rebuildable projection** — the line-offset index, the leaf-path
  slice the executor consumes, the Markdown export, the vector index, the recency/list index are all
  *derived from* the log. So parallel storage is neither a consistency liability nor a real cost.
- **Messages are a DAG (`id` + `parent_id`), not a `Vec`.** Linear chat is the degenerate case. This
  is the seam that makes branching / loop-back / debate additive. The executor still sees a flat
  leaf-path slice, so it never changes. Conversation headers carry **lineage**
  (`parent_conversation`, `spawned_by`) for subagent trees; nodes carry an **`author`** identity (not
  just user/assistant/tool) for multi-agent/debate.
- **Node ids are time-sortable (ULID/UUIDv7), assigned at append time.** *This is the one choice that
  can't be retrofitted.* It makes the log intrinsically id-sorted, so random node lookup is
  O(log n) binary-search over a line-offset array (parents always earlier → seek backward only), with
  no persisted secondary index. Random UUIDv4 would force a real maintained index. The control plane
  genuinely needs this lookup: a branch can outgrow the context window while orchestration must still
  resolve an arbitrary earlier fork node.
- **Daemonless by default, on purpose.** The v1 impl is JSONL; **SQLite (WAL)** is a drop-in
  graduation (one process, real index/FTS, still daemonless); **Postgres + pgvector** is a swap-in
  *only if* we ever go multi-process/multi-tenant (it also folds in vectors); **DuckDB** is an
  analytics sidecar over the JSONL, not a store. A background-daemon DB is *anti-modular* — it would
  drag a running server into every composition of the crate set, against the "glue into LibreChat or
  an autonomous agent" substrate goal. The trait lets the rare multi-process deployer opt into
  Postgres without touching the agent loop.
- **Concurrency = per-conversation single-writer actor.** Participants (user, subagents, debaters)
  *send* to the conversation; the actor serializes appends (safe regardless of line size) and
  persists a node **only when complete** (so a cancelled streaming turn is a clean no-op on disk —
  the `turn_stream` rollback stays purely in-memory). Interruption is a control message — the
  generalization of the stream-cancel primitive already built. Different conversations are
  independent logs (no contention), which is the *only* "concurrency" the foreseen features actually
  need — none of them require multiple OS processes.
- **Search**: at API-request scale, ripgrep over JSONL is functionally equivalent to a DB index, so
  search performance gets zero weight; the long-conversation case is a *retrieval* problem (vector +
  recency projections), not a faster-traversal problem.

**Decided now**: JSONL-outside-vault; append-only log of DAG nodes; **sortable ids assigned at
append**; the `ConversationStore` trait; single-writer-per-conversation. **Deferred (additive, no
schema change)**: the persisted index, the Markdown/vector/recency projections, SQLite/Postgres/
DuckDB impls, and the branching/debate/parallel UX.

---

## Tier 1 (matured vision, 2026-06-26): Modularity & Mesh

These two decisions record the matured architectural vision agreed in the 2026-06-26 planning
session. They are load-bearing because they reframe the substrate (event-driven, vault-optional) that
every later feature builds on. See the three pillars in
[`docs/architecture/overview.md`](../architecture/overview.md), the thesis in
[`docs/architecture/positioning.md`](../architecture/positioning.md), the seam plan in
[`docs/architecture/modularity.md`](../architecture/modularity.md), and the mesh source in
[`docs/ideas/meshify.md`](../ideas/meshify.md).

### 18. Incremental Event-Bus Mesh (with checkpoints)
**Why it matters**: The single enabler for the whole modularity vision — vault-optional, multiple
dispatchers/executors, cron, partial deploys, self-improvement-as-a-service — is that components
publish/subscribe events rather than calling each other directly. *How* we get there determines
whether the substrate ships at all.

**Decision**: Adopt [`meshify.md`](../ideas/meshify.md)'s direction — components publish/subscribe
events rather than calling each other — but **incrementally**, NOT as a big-bang refactor. Wrap seams
behind an `EventBus` trait **as they are touched**; **new components are bus-native from day one**;
old ones migrate when next touched (the chat -> dispatcher wiring in roadmap Phase 1 is the first
seam). Safety (narrowing, zone checks, provenance stamping, magnitude gates) stays in the bus layer —
services only consume or produce events the bus has already validated.

**Guard against drift** with concrete "the mesh is real now" checkpoints tied to features, so the
substrate doesn't quietly stall:
- **Checkpoint #1 (Phase 1)** — the capability catalog is a **live, bus-queryable registry**, not
  static config (the same registry the TUI/WebUI query).
- **Checkpoint #2 (Phase 2)** — the **coding-agent is a bus service**; an MCP hot-reload re-registers
  in the catalog.
- **Checkpoint #3 (Phase 3)** — **cron and vault-watch are interchangeable event-sources**, and a
  second dispatcher/executor is **config-enableable**.

**Rationale**: The mesh is the single enabler for the modularity vision (vault-optional, multiple
dispatchers/executors, cron, partial deploys, self-improvement-as-a-service). A foundation-first
build risks months of plumbing with nothing shipped; incremental-with-checkpoints gets the substrate
as a **side effect of feature work** while the checkpoints keep it honest. The public HTTP/SSE API and
the TUI client never change during the migration.

**Status**: Decided (2026-06-26). Realized incrementally across roadmap Phases 1–3.

### 19. TurboVault as Privileged Plugin, not Hard Dependency
**Why it matters**: The original Pillar 1 ("the vault is the source of truth") read as a system-wide
invariant. The matured pillars demote it: the core must be usable without TurboVault, or the
"modular MCP/hook substrate" pillar and the general-MCP-agent milestone are not real.

**Decision**: **The vault (TurboVault) is the default, privileged perception+storage plugin, not a
hard dependency.** The core — dispatch / execute / MCP runtime / chat / conversation-store — is
**vault-agnostic**. The vault's coupling is isolated to the **reactive subsystem** (watch +
provenance loop-breaking), which becomes *the vault plugin* behind an **event-source / hook trait**
(the same trait cron implements — Decision 18). Privileged-default in the meantime: TurboVault stays
the out-of-the-box perception+storage layer, but nothing in the core path requires it. This is the
destination the mesh (Decision 18) reaches; vault-decoupling lands in roadmap Phase 3.

**Supersedes**: the earlier framing that "the vault is the source of truth" as a system-wide
invariant (see the dated clarifying note on Decision 17 above). Pillar 1 in
[`docs/architecture/overview.md`](../architecture/overview.md) now reads "vault = default
perception+storage plugin"; [`docs/architecture/positioning.md`](../architecture/positioning.md)
states the differentiation this unlocks.

**Status**: Decided (2026-06-26). Privileged-default now; hard-plugin via the event-source trait in
Phase 3.

---

## Tier 4: Lower-Regret / Polish Decisions

- **A2A (Agent2Agent) protocol interop** — not yet a decision, captured as
  [`a2a-protocol-idea.md`](../ideas/a2a-protocol-idea.md) (2026-07-01). Preliminary read: the
  Decision 17 conversation-store seams (`author`, lineage) and the Decision 18 mesh direction
  already carry most of the data-model need; the open gap is a new inbound protocol surface and
  an outbound peer-delegation capability, gated like any other MCP/subagent trust boundary. Not
  before Phase 3.
- Exact initial model/provider and SDK choice (DeepSeek route, config approach).
- ~~Precise naming for the enhanced liberado component~~ **Resolved**: split into `liberado-dispatcher` (new out-of-band routing agent) + `liberado-memory-mcp` (renamed `liberado-tool-helper-mcp`, the mem0-backed general + procedural memory store the dispatcher consumes). Actual directory rename happens at implementation time (planning phase keeps the existing folder name).
- v1 scope boundaries (what is explicitly deferred).
- Documentation location for system prompts and dispatch logic (vault vs code).

---

## Next Actions

**All decisions resolved — Tier 1 (1–5), Tier 2 (6–12), Tier 3 (13–16), Decision 17, the matured-vision
mesh/modularity decisions (18–19, 2026-06-26), and the Tier 4 naming item.**

Companion specs:
- `liberado-permissions-idea.md` — Decision 4 (capability/zone model)
- `liberado-vault-concurrency-spec.md` — Decision 5 (provenance, loop-breaking)
- `liberado-dispatch-logic-spec.md` — Decision 1 (routing) + Decisions 8 interfaces
- `liberado-context-policy-spec.md` — main-agent context (deliberately dumb header)
- `liberado-inbox-spec.md` — async capture + ambient analysis
- `liberado-vault-maintenance-and-git-spec.md` — git backstop + maintenance tasks
- `liberado-config-spec.md` — Decision 14 (config topology)
- `liberado-testing-and-eval-spec.md` — Decision 16 (integration-test harness)
- `liberado-conversation-store-spec.md` — Decision 17 (conversation history store)

Remaining Tier 4 (lower-regret, can settle during implementation): exact initial model/provider + SDK choice; v1 scope boundaries; doc location for system prompts.

These two steps are realized (June 24, 2026):
1. **Core shared types** — `crates/common` holds the full type vocabulary (provenance, capability, dispatch, event, model, config, proposal).
2. **V1 vertical slice** — The daemon→dispatcher→orchestrator→executor pipeline is end-to-end wired, tested, and the proposal approve→execute loop is closed.

This log is updated after each decision is resolved.