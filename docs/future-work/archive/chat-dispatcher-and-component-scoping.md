# Chat -> dispatcher -> orchestrator, MCP fleet wiring, component-scoped grants

**Status**: Done (2026-07-02). All three phases implemented, full workspace test suite green,
and verified live against the real daemon + a real chat turn (see Verification below).

## Context

Three things came out of a roadmap-reading conversation, in an order that turned out to be
load-bearing, not arbitrary:

1. **Chat bypassed the dispatcher.** `docs/roadmap.md`'s Phase 1 said so directly: "Today
   chat drives the executor directly, bypassing the tool-advisor, the guards, and sub-delegation."
   Confirmed in code: `ChatSessions::turn`/`turn_stream` called `Conversation::turn`/`turn_stream`,
   which called `Executor::converse_stream` directly — `Dispatcher`/`Orchestrator` never entered
   the chat path at all.
2. **Wiring in the rest of the Liberado MCP fleet** (`liberado-mcp-forge`) for realistic testing was
   nearly pointless to do first — with only 1-2 MCPs registered there's no meaningful "which MCPs
   are visible where" to test, and with chat bypassing the dispatcher, added MCPs would only ever
   be reachable through the *unscoped* executor path anyway.
3. **Dispatcher-only vs main-agent-visible MCP separation** maps onto `Grant.component`
   (`crates/common/src/config.rs`), which existed in the data model but was discarded —
   `Policy::base_capabilities()` unioned every grant regardless of `component`, with a doc comment
   admitting it: *"v1 unions across components; per-component narrowing for subagents is a later
   refinement."* This is Phase 1's third bullet, "Multi-MCP + parallel, capability-narrowed
   sub-delegation" — not a fourth, competing idea.

Hence the order: route chat through the dispatcher first (so "dispatcher-only" is a real, reachable
concept) -> wire in the fleet (gives real breadth to test against) -> component-scope the grants
(make the visibility split real).

**Architecture decision resolved before implementation**: `Orchestrator::run` has no streaming — it
calls `Executor::execute` (report mode), which blocks until a full `Report` is ready, unlike
`Executor::converse_stream`'s token-by-token `AgentEvent`s. Straight substitution would have killed
SSE streaming for every dispatcher-routed turn. Decided (and implemented): `ExecuteDirect` (the
common case) keeps using the existing streaming `converse_stream` path — zero UX regression;
`DispatchSubagent` (reserved for complex/open-ended goals) routes through the existing non-streaming
`Orchestrator::run`, surfaced as a status message then the final report. `Clarify`/`Propose` are
immediate, no execution, no streaming concern either way.

---

## Phase A — Route chat through the dispatcher

`crates/main-agent/src/sessions.rs`: `ChatSessions` gained `dispatcher: Option<Dispatcher>`,
`dispatch_catalog: Vec<liberado_dispatcher::McpDescriptor>`, and `orchestrator: Option<Orchestrator>`
fields, set together via a new `with_dispatch(dispatcher, catalog, orchestrator)` builder
(orchestrator is required, not optional, alongside a dispatcher — see below for why). A private
`dispatch_turn(user) -> DispatchOutcome` runs before both `turn` and `turn_stream`'s existing
execution:

1. Builds a `DispatchRequest { goal: user_message, catalog: self.dispatch_catalog.clone(),
   capabilities: self.capabilities.clone(), reaction_depth: 0 }` — reusing the *same*
   `CapabilitySet` already flowing into `ChatSessions`' own `ScopedRuntime` (via `with_guards`), so
   there's no extra coupling to `Policy::capabilities_for` needed at this layer; whatever component
   `build_chat` passes into `with_guards` is automatically what dispatch requests get checked
   against too.
2. Calls `dispatcher.dispatch(&req).await`.
3. `ExecuteDirect` -> `DispatchOutcome::Proceed`, falling straight into the *existing*
   `Conversation::turn`/`turn_stream` call, unchanged. This is what preserves streaming.
4. Everything else (`Clarify`, `Propose`, `DispatchSubagent`) -> `orchestrator.run(decision, user,
   &correlation_id)`, mapped to a plain string reply (`DispatchOutcome::Answered`):
   - `Clarify` -> the questions, formatted as prose.
   - `Reported(report)` -> `report.summary`.
   - `Propose(proposal)` -> written via a new `write_chat_proposal` (same pattern as
     `RiskGatedToolRuntime::write_proposal` already used: plain `tokio::fs::write` under
     `proposals_dir/proposals/<id>.md`, not a vault write — chat proposals already lived outside
     the vault so a vault watcher never reacts to them).
5. `Conversation` gained a small `answer(user, reply)` method (push both messages directly, no
   executor involvement) so a dispatch-resolved turn still persists like any other.

`crates/server/src/lib.rs`'s `build_chat` constructs the `Dispatcher`/`Orchestrator` and calls
`with_dispatch`, but only when an MCP registry exists — mirroring `configure_daemon`'s
`orchestrator_attached = dispatcher_attached && mcp.is_some()`. No MCP configured => chat skips
dispatch entirely and runs exactly as it did before this change.

**No changes needed to `Dispatcher`'s structure** — it already takes `CapabilitySet` per-call via
`DispatchRequest`, not stored on the struct. `Orchestrator` did need a structural change — see
Phase C.

---

## Phase B — Wire in the MCP fleet

Fixed a broken join found while researching this: the live config (`config/topology.toml`) had
exactly one `[[mcps]]` entry, named `tasks-mcp`, using a **hand-written pre-forge stdio path**
straight to `liberado-tool-helper-mcp`'s binary — a leftover from before `liberado-mcp-forge`
existed. Renamed it to `liberado-tool-helper-mcp` with `transport = { kind = "managed" }`, added
`liberado-weather-mcp`/`liberado-pdf-mcp`/`liberado-rentcast-mcp`/`liberado-anythingllm-mcp`,
created the live `config/mcp-sources.toml` (didn't exist yet), and ran `liberado-mcp-forge sync` —
all 5 binaries built and landed at their resolved paths.

**Deferred, not this pass**: `liberado-caldav-mcp` (README suggests HTTP-only; `Managed` transport
is stdio-only by design) and `liberado-calorie-counter-mcp` (its real binary is named `liberado-mcp`,
not something calorie-counter-specific — needs a one-line upstream `Cargo.toml` fix, a repo you own,
cheap to do later).

### A real bug found during live verification

Wiring in the fleet and actually starting the daemon against it (not just confirming the binaries
build) surfaced a genuine bug: connecting all registered MCPs made chat time out after 60s and fall
back to **zero tools** — not a partial degradation. Diagnosed by isolating each server:

- `liberado-rentcast-mcp` / `liberado-anythingllm-mcp` need `RENTCAST_API_KEY` /
  `ANYTHINGLLM_API_KEY` (expected — not configured yet). Disabled (`enabled = false`) with a comment
  until the keys exist.
- `liberado-weather-mcp` — not a credentials issue. Running the binary directly and piping it a raw
  MCP `initialize` request showed it **defaults to HTTP transport** (binds `0.0.0.0:8000`) and
  ignores `MCP_TRANSPORT=stdio`. `McpTransport::Managed` only ever spawns via stdio
  (`liberado-mcp-forge`'s v1 is deliberately stdio-only — see `crates/mcp-forge/ARCHITECTURE.md`'s
  non-goals), so this server can't be used as `managed` until it actually supports a stdio mode.
  Disabled with a comment explaining exactly this, pending an upstream fix.
- `liberado-tool-helper-mcp` and `liberado-pdf-mcp` — confirmed working correctly over stdio (tested
  directly, and live via the daemon). With just these two enabled, chat connected in under a second
  with **21 real tools**.

This meant the important lesson wasn't in the code changes — it was that "the build succeeded" is
not the same claim as "the binary speaks the transport `Managed` assumes," and that discovering the
gap requires actually starting the daemon, not just running `liberado-mcp-forge sync`.

---

## Phase C — Component-scoped capability grants

`crates/common/src/config.rs`: `Policy::base_capabilities()` replaced with
`Policy::capabilities_for(component: &str) -> CapabilitySet` — unions only the grants whose
`component` matches. Two call sites updated: `crates/bootstrap/src/lib.rs`'s `configure_daemon`
(`capabilities_for("dispatcher")`) and `crates/server/src/lib.rs`'s `build_chat`
(`capabilities_for("main-agent")`).

**Closed a real gap found during research, not just renamed the method**: `Orchestrator`'s
`ExecuteDirect` arm called `self.factory.runtime_for(&[], provenance)` — an *empty* allow-list,
which `ScopedRuntime`/`McpRegistry` treat as "every registered MCP is visible." So even before this
change, a `DispatchSubagent`-restricted MCP was still reachable the moment any goal resolved to
`ExecuteDirect`. Fixed: `Orchestrator::new` now takes a third `capabilities: CapabilitySet`
parameter; `ExecuteDirect` computes `allowed_mcps` from its granted `ExecuteMcp` names instead of
passing `&[]`. A second, smaller gap surfaced while testing the fix: an *empty* `allowed_mcps` from
zero grants would *also* mean "everything" to the factory (the exact same footgun, one level down)
— fixed with a small `NoMcpRuntime` (mirrors the `NoToolsRuntime`/`NoTools` pattern already used
elsewhere) so zero grants correctly means zero tools, not "ask the factory for everything."

`config/policy.toml` (and `config.example/policy.toml`) split their single `component = "agent"`
grant into `"main-agent"` and `"dispatcher"`, duplicating existing capabilities to both so nothing
regressed on migration, then granting `liberado-rentcast-mcp` (live) / `code-dispatch` (example)
to `"dispatcher"` only — the concrete illustration of an MCP reachable via dispatch-routed
execution but never visible directly in chat's own tool surface.

---

## Files touched

- `crates/common/src/config.rs` — `Policy::capabilities_for`; tests.
- `crates/common/src/event.rs` — considered adding `event_source::CHAT` per the original plan, but
  `DispatchRequest` has no `Event` field at all (it takes a plain `goal: String`), so there was
  nothing to attach it to — skipped as unused scope, not a decision to revisit lightly.
- `crates/main-agent/src/lib.rs` — `Conversation::answer`.
- `crates/main-agent/src/sessions.rs` — `ChatSessions` dispatch fields, `with_dispatch`,
  `dispatch_turn`, `write_chat_proposal`, `format_questions`; `turn`/`turn_stream` updated; 3 new
  tests (`clarify_decision_answers_without_executing`,
  `execute_direct_decision_falls_through_to_normal_execution`,
  `propose_decision_writes_a_proposal_file_and_confirms`).
- `crates/orchestrator/src/lib.rs` — `Orchestrator::new` gains `capabilities`; `ExecuteDirect` arm
  scopes `allowed_mcps` instead of passing `&[]`; `NoMcpRuntime`; all call sites (including
  `crates/orchestrator/tests/orchestrate.rs`, `crates/daemon/src/lib.rs`) updated; 2 new tests
  (`execute_direct_scopes_the_runtime_to_the_granted_mcps`,
  `execute_direct_with_zero_grants_never_calls_the_factory`).
- `crates/bootstrap/src/lib.rs` — `configure_daemon` uses `capabilities_for("dispatcher")`, passes
  it into `Orchestrator::new`.
- `crates/server/src/lib.rs` — `build_chat` computes `capabilities_for("main-agent")` up front,
  constructs `Dispatcher`/`Orchestrator`, calls `with_dispatch`.
- `crates/daemon/src/lib.rs` — test call sites updated for the `Orchestrator::new` signature change
  (no production logic changed here).
- `config/topology.toml`, `config.example/topology.toml` — fleet entries; weather/rentcast/anythingllm
  disabled with reasons.
- `config/policy.toml`, `config.example/policy.toml` — component-split grants.
- `config/mcp-sources.toml` (new, live), `config.example/mcp-sources.toml` — fleet sources.

## Verification

- `cargo build --workspace` and `cargo test --workspace`: zero failures, run after each phase and
  again after all three landed together.
- `liberado-mcp-forge sync` against the live config: all 5 binaries built and installed.
- `liberado config check`: 2 grants, 5 mcps, valid.
- Live daemon start against the real vault + real DeepSeek key: `chat: connected MCP tools`,
  `chat: tool runtime ready count=21`, `dispatcher capability boundary configured from policy
  grants=2 capabilities=10`, `orchestrator enabled`.
- A real chat turn over `POST /api/chat/stream` ("what tools do you have access to?"): full
  token-by-token SSE streaming (confirmed — dozens of individual `event: token` frames, not one
  batched reply), the model correctly listed all 21 tools from both connected MCPs, clean `done`
  terminator. This is the direct confirmation that `ExecuteDirect` routing through the dispatcher
  did not regress the streaming UX.
