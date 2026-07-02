# Hygiene Audit — Duplication, Cohesion, and Additional Coupling (2026-07-02)

**Purpose**: After two hardening passes this session (best-effort MCP boot connection,
`b1d5e3f`/`1dcd0cd`; runtime-level tool gating, `6240e4d`), the second one surfaced a real crate-coupling
smell by accident (`liberado-mcp` depending on `liberado-orchestrator`, which nearly forced a circular
dependency and had to be worked around by relocating `RiskGatedToolRuntime` into `liberado-executor`).
That prompted a deliberate, broader hygiene pass — not fixing anything yet, mapping it first — before
resuming feature/hardening work on top of a foundation that's grown a lot of new abstractions quickly
this session (`CapabilityCatalog`, `DispatchTuning`, `RuntimeFactory`, `McpRegistry`, `ScopedRuntime`,
`RiskGatedToolRuntime`, `Orchestrator::gate`).

**Companion doc**: [`crate-modularity-audit.md`](crate-modularity-audit.md) already covers the crate
dependency graph in depth, from an earlier, differently-motivated pass (the "could this be a reusable
coding-agent substrate" bar). Its **Finding 4** (`liberado-mcp` depends sideways on
`liberado-orchestrator` for the `RuntimeFactory` trait) is the *same* issue this session's coupling
agent re-found independently — good cross-confirmation, not a new discovery. Its **Finding 5**
(`liberado-mcp` bundling `risk_gated.rs`) is now **partially resolved**: `risk_gated.rs` moved out of
`mcp` into `liberado-executor` as part of this session's runtime-gating work — not into "its own crate"
as that finding's fix suggested, but the effect (mcp no longer carries the safety-gating type surface)
is the same. `mcp`'s dependency on `orchestrator` itself (the actual Finding 4 root cause) is
**still open** — see item 6 below, which is the same fix that doc recommends.

This doc covers what that one doesn't: duplication (test-mock and production-logic copy-paste) and
function cohesion, plus a handful of coupling observations the dependency-graph-only pass didn't
surface.

**Method**: three parallel research passes — (1) crate dependency graph + coupling, (2) duplication
across test and production code, (3) function cohesion + dead code, cross-checked against a live
`cargo build --workspace` for the warning list.

---

## Tier 1 — small, low-risk (in progress, same session)

1. **Delete `SilentRuntime`/`SilentFactory`** (`crates/daemon/src/lib.rs:917-943`) — an exact
   copy-paste of `RecordingRuntime`/`RecordingFactory` ~100 lines above it in the *same file* (same
   `Arc<Mutex<Vec<ToolInvocation>>>` field, same `push` logic, only the name differs).
2. **Two stray unused imports**: `Consequence` in `crates/dispatcher/src/lib.rs:26`, `McpDescriptor`
   in `crates/daemon/src/lib.rs:20` (the latter is only used inside a test, which imports it locally).
3. **Consolidate consequence-catalog construction** — `crates/server/src/lib.rs:236-240`
   (`build_chat`) and `crates/bootstrap/src/lib.rs:123-127` (`configure_daemon`) both build the
   identical `Vec<(String, Consequence)>` from `catalog.descriptors()`. Add
   `CapabilityCatalog::consequence_catalog()` in `liberado-common`; both call sites become one line.
4. **Consolidate `LIBERADO_DATA_DIR`/proposals-dir resolution** — same two files, same duplicate
   (`std::env::var("LIBERADO_DATA_DIR").unwrap_or_else(|_| ".liberado".into())` then
   `.join("proposals")`). Add a `liberado_bootstrap::data_dir() -> PathBuf` helper (parallel to the
   crate's existing `config_dir()`); `liberado-server` already depends on `liberado-bootstrap`, so no
   new dependency edge.
5. **Consolidate granted-MCP extraction from `CapabilitySet`** — `crates/orchestrator/src/lib.rs:210-218`
   and `crates/main-agent/src/sessions.rs:388-396` each hand-roll the same `filter_map` over
   `Capability::ExecuteMcp(name) => Some(name.clone())`. Add `CapabilitySet::granted_mcps()` in
   `liberado-common`.

**Status**: Done (2026-07-02) — all 5 executed as one batch, plus two follow-on fixes the batch itself
surfaced: two `use super::*`-dependent test-module imports (`Consequence` in `crates/dispatcher/src/lib.rs`'s
`mod tests`, `McpDescriptor` in `crates/daemon/src/lib.rs`'s `mod tests`) needed to move from the
now-trimmed module-level import down into the test module itself, since a plain `cargo build` doesn't
compile `#[cfg(test)]` code and so didn't catch that the "unused" module-level imports were actually
load-bearing for tests. Verified with a full `cargo build --workspace` + `cargo test --workspace`
(zero failures, 51 suites) after each step.

---

## Tier 2 — structural, moderate scope (not yet started)

6. **Move the `RuntimeFactory` trait from `liberado-orchestrator` to `liberado-executor`.** Root cause
   of the near-cycle patched around this session (relocating `RiskGatedToolRuntime` instead of the
   trait). `liberado-mcp` depends on `liberado-orchestrator` *solely* for this trait; moving it to
   `liberado-executor` (which both crates already depend on, and which already owns the `ToolRuntime`
   trait these factories build) removes the layering violation at the source instead of leaving the
   next collision to be discovered by accident again. Same fix `crate-modularity-audit.md`'s Finding 4
   already recommends — this doc doesn't repeat its reasoning, just confirms it's still open.
7. **Split `build_chat`** (`crates/server/src/lib.rs:210-313`, ~100 lines, 5 distinct concerns:
   guard-context assembly, MCP connection + orchestrator construction, session-store setup, dispatch
   wiring) into named helpers — a `build_guard_context(...)` (which becomes the natural home for items
   3-4's new shared calls) and a `connect_chat_runtime(...)`. `configure_daemon` itself is fine as-is
   (~50 lines, single-responsibility) once it calls the same shared helper instead of duplicating the
   guard-context computation inline.
8. **Consolidate `RecordingRuntime`/`RecordingFactory`** — duplicated field-for-field in
   `crates/orchestrator/tests/orchestrate.rs:34-86`, `crates/orchestrator/src/lib.rs`'s own inline
   `#[cfg(test)]` module (`:563-595`), and twice more in `crates/daemon/src/lib.rs` (`:815-840`, and
   the `SilentRuntime`/`SilentFactory` pair item 1 already flags as a pure duplicate of this same
   shape). One shared fixture (a small `#[cfg(test)]` module re-exported from `liberado-orchestrator`,
   or a dedicated `liberado-test-support` crate) instead of 4+ copies. The trivial `Noop*`/`Unused*`
   stub runtimes (2-4 lines each, several locations) are *not* worth this treatment — too small for the
   abstraction to pay for itself.

---

## Tier 3 — worth a conversation, not urgent

9. **Merge `liberado-theme` + `liberado-markdown` + `liberado-commands`** — three tiny,
   zero-Liberado-logic UI-support crates, always consumed together by `tui`/`webui`/`cli`. Merging
   would cut three Cargo workspace members and the `commands -> theme` inter-crate edge without losing
   a real boundary.
10. **Split `liberado-bootstrap`** so `liberado-mcp-forge` (a build tool) doesn't transitively pull in
    `liberado-daemon`'s vault-watching machinery just to load config and wire MCPs. Low urgency —
    `mcp-forge` still builds and works today, this is a compile-time/conceptual-surface concern, not a
    correctness one.
11. **Whether `liberado-server`'s assembly logic in `lib.rs` duplicates or legitimately augments
    `liberado-bootstrap`'s stated assembly role** — flagged as ambiguous by the coupling pass, needs a
    closer read of both files side by side before judging either way.
12. `liberado-daemon` depending on `liberado-orchestrator` without any corresponding `liberado-mcp`
    dependency (production wiring happens from outside, via `bootstrap`/`server`) — this coupling is
    invisible from `daemon`'s own `Cargo.toml` alone. Low urgency; a doc-comment note on `Daemon`'s
    orchestrator field would make the implicit requirement explicit.

---

## Confirmed fine, leave alone

- **All 24 `liberado-webui` "never used" warnings** — verified via `git log`/`git status`: committed,
  active WIP on the current `ui-polish` branch (the `3bdb8a9` "flesh out sidebar, MCP panel, markdown
  rendering, slash commands, chat UX" commit). Not dead code — the Dioxus component bodies that call
  these helpers just haven't been wired up yet.
- **`chat-client-contract`'s separateness from `liberado-server`** — intentional: it exists specifically
  so WASM clients (`webui`) can compile against wire types without pulling in `tokio`/server-side deps.
  Do not merge, per `crate-modularity-audit.md`'s own note that this crate already passes its "could
  someone use just this crate?" test.
- **`liberado-conversation-store` depending on `liberado-provider`** — reuses `provider`'s
  `Message`/`Role` types as the stored node payload rather than defining separate wire types. Pragmatic,
  not a real coupling problem; only worth revisiting if `provider`'s message vocabulary and
  conversation-history node types ever need to diverge.
- **The trivial `Noop*`/`Unused*` test stub runtimes** — too small (2-4 lines) to justify shared
  infrastructure; see item 8.

---

## Recommended sequencing

1. **Tier 1 (items 1-5)** — safe, mechanical, independently confirmed by two of the three research
   passes. Execute now, in one batch.
2. **Item 6** (`RuntimeFactory` relocation) — highest-leverage structural fix; closes the actual root
   cause behind a workaround already shipped this session. Natural next pick after Tier 1.
3. **Items 7-8** — pair naturally with item 6's crate-boundary work (item 7 especially, since it
   consumes items 3-4's new shared helpers).
4. **Tier 3** — revisit after 1-2 and discuss before committing to any of it; none of it is blocking
   anything else on this list.
