# Phase 1 wrap-up: live capability catalog + dispatcher-narrowed tool surfacing

**Status**: Done (2026-07-02). Both sub-projects implemented, full workspace test suite green,
verified live against the real daemon and real chat turns over DeepSeek (see Verification below).

## Context

Phase 1's last open roadmap bullet was "Live capability catalog + on-demand tool surfacing — the
validated lazy-loading pattern; the token-efficiency core." Research before implementation found
it was really two sub-projects:

1. **The system was 100% boot-time-static end to end.** `topology.toml` -> `catalog_from_config`
   produced a `Vec` copied independently into three places (the daemon's reactive dispatch, chat's
   own dispatch, and the server's `/api/catalog`) via two duplicate descriptor types
   (`liberado_dispatcher::McpDescriptor` and `liberado_common::McpDescriptor` — identical shape
   except the latter's `provenance` field). `CapabilityCatalog`'s watch-channel "live" machinery had
   zero production subscribers. So "live" meant *unify the three static copies into one shared
   source*, not "react to changes that don't exist yet" (MCP registration itself is still
   boot-time-fixed; that's separate, larger scope, not attempted here).
2. **Every granted MCP's full tool schema was sent to the model every turn, unconditionally.**
   `ScopedRuntime` narrowed by MCP *server* name but always returned the full JSON-Schema
   `parameters` for every tool of every allowed server — at the (then) 21-tool fleet, roughly
   1,000-2,000 tokens of pure overhead per turn regardless of what the goal actually needed. The
   dispatcher's own `ExecuteDirect` decision did nothing to narrow this: `ChatSessions` scoped only
   by the static `"main-agent"` grant, ignoring what the dispatcher had just decided.

Decided: dispatcher-narrowed scoping as the default, configurable (not hardcoded) so falling back
to "always full grant" is a `tuning.toml` edit, not a code change — not the alternative two-stage
meta-tool lazy-load pattern (bigger build, adds a round-trip per first tool use each session).

---

## Sub-project 1 — Catalog unification

`liberado_dispatcher::McpDescriptor` was deleted; `crates/dispatcher/src/lib.rs` now does
`pub use liberado_common::McpDescriptor;` — every existing `liberado_dispatcher::McpDescriptor`
import path (daemon, eval, dispatcher's own `guards` module) kept working unchanged, since it's a
re-export of the same type now, not two types with the same shape.

`catalog_from_config` (`crates/bootstrap/src/config.rs`) returns `Vec<liberado_common::McpDescriptor>`
directly. A new `capability_catalog_from_config(config) -> CapabilityCatalog` builds the live object
from those descriptors — the one place a `CapabilityCatalog` gets constructed from config now.

**The sharing is real, not just deduplicated types.** `crates/server/src/lib.rs::run()` builds ONE
`Arc<CapabilityCatalog>` and threads it into `configure_daemon` (new parameter, replacing its
internal `catalog_from_config` call) and `build_chat` (same). `DispatcherContext.catalog`
(daemon) and `ChatSessions.dispatch_catalog` (chat) changed from an owned `Vec` frozen at
construction to holding the `Arc<CapabilityCatalog>` handle, calling `.descriptors()` fresh inside
`dispatch_request()`/`dispatch_turn()` on each call instead of once at boot. Nothing mutates the
catalog after boot today, but the daemon, chat, and API now all read the *same* object on every
dispatch — the real hook for later runtime registration (e.g. self-extension) to reach every
consumer without each one needing its own wiring.

## Sub-project 2 — Dispatcher-narrowed tool surfacing

`DispatchAction::ExecuteDirect` gained `relevant_mcps: Vec<String>` (`#[serde(default)]`,
`crates/common/src/dispatch.rs`) — mirrors `DispatchSubagent.allowed_mcps`. The dispatcher's
classify prompt (`crates/dispatcher/src/lib.rs`) asks for it alongside the existing
`DispatchSubagent` guidance.

`DispatchTuning.narrow_direct_tools: bool` (default `true`) is the single point of control,
deliberately placed on the *dispatcher* side: `Dispatcher::dispatch()` clears `relevant_mcps` to
empty post-classification when the tunable is off (the same deterministic post-processing pattern
the guard pipeline already uses for downgrades), so every downstream consumer has one rule —
"respect `relevant_mcps` when non-empty, else use the full grant" — with zero awareness of the
toggle itself.

`guards::evaluate`'s `referenced_mcps` (`crates/dispatcher/src/guards.rs`) now also checks
`ExecuteDirect.relevant_mcps` against the request's capabilities — the same capability-gap
protection `seed_calls` and `DispatchSubagent.allowed_mcps` already got. A hallucinated or
out-of-scope name is caught there, not silently trusted downstream.

Two consumers apply the same rule, structurally separate (chat's `ExecuteDirect` never touches the
orchestrator, staying on the streaming path; the daemon's reactive path always does):
- `crates/orchestrator/src/lib.rs`'s `ExecuteDirect` arm intersects `relevant_mcps` with the
  already-capability-scoped ceiling (the fix from the prior phase that closed the "empty allow-list
  means everything" gap) rather than replacing it — narrows further, never widens.
- `crates/main-agent/src/sessions.rs`: `DispatchOutcome::Proceed` now carries the decision's
  `relevant_mcps`; `build_turn_runtime` intersects it with the granted MCP set the same way.

---

## Files touched

- `crates/common/src/dispatch.rs` — `ExecuteDirect.relevant_mcps`.
- `crates/common/src/config.rs` — `DispatchTuning.narrow_direct_tools`.
- `crates/dispatcher/src/lib.rs` — `McpDescriptor` re-export; classify prompt;
  `enforce_narrow_direct_tools`; `DispatchRequest.catalog` retyped.
- `crates/dispatcher/src/guards.rs` — `referenced_mcps` covers `relevant_mcps`.
- `crates/orchestrator/src/lib.rs` — `ExecuteDirect` arm narrows further by `relevant_mcps`.
- `crates/daemon/src/lib.rs` — `DispatcherContext.catalog` is `Arc<CapabilityCatalog>`.
- `crates/main-agent/src/sessions.rs` — `DispatchOutcome::Proceed` carries `relevant_mcps`;
  `build_turn_runtime` narrows; `dispatch_catalog` is `Arc<CapabilityCatalog>`.
- `crates/bootstrap/src/lib.rs`, `crates/bootstrap/src/config.rs` — `configure_daemon` takes the
  shared catalog; `capability_catalog_from_config` added.
- `crates/server/src/lib.rs` — builds the one `Arc<CapabilityCatalog>`, threads it through, deletes
  the manual per-item translation loop that used to build a second copy.
- `crates/eval/src/main.rs`, `crates/common/tests/coverage.rs` — mechanical field/type updates.

## Explicit non-goals

- Two-stage meta-tool lazy-loading — not built; dispatcher-narrowing chosen instead.
- Actual runtime MCP registration/deregistration reaching a live daemon without a restart — the
  catalog sharing sets up the hook, but connector/registry construction is still boot-time-fixed.
- New eval scenarios for token-cost / tool-selection-with-noise — no existing instrument for this;
  worth building later, not required to ship the narrowing itself.

## Verification

- `cargo test --workspace`: zero failures, run after each sub-project and again after both landed.
- New tests: `guards::execute_direct_requires_relevant_mcps_granted` /
  `execute_direct_with_only_granted_relevant_mcps_passes`; dispatcher's
  `narrow_direct_tools_default_keeps_relevant_mcps` / `narrow_direct_tools_off_clears_relevant_mcps`;
  orchestrator's `execute_direct_relevant_mcps_narrows_within_the_granted_ceiling`; sessions'
  `execute_direct_relevant_mcps_narrows_the_surfaced_tools` /
  `execute_direct_empty_relevant_mcps_falls_back_to_full_grant`.
- **Live, not just unit-tested**: started the daemon against the real vault with `liberado-pdf-mcp`
  and `liberado-tool-helper-mcp` connected (both granted to `"main-agent"`), with debug tracing on.
  A PDF-related goal traced `chat turn tool scope count=1 mcps=["liberado-pdf-mcp"]`; a
  memory-search goal traced `count=1 mcps=["liberado-tool-helper-mcp"]` — the dispatcher correctly
  narrowed to exactly the relevant MCP each time, out of a two-MCP grant, over the real model.
  `/api/catalog` confirmed unaffected by the unification refactor (still returns both MCPs' full
  tool breakdown correctly).
