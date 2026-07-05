# Crate Modularity & Deduplication Audit

**Purpose**: Answer a concrete bar the project owner set (2026-07-01): *"I don't want to build a
terminal coding-agent scaffold like Claude Code out of these components, but I'd like to be able
to."* This is a dependency-graph and coupling audit of the whole workspace against that bar, plus
the duplication it turned up along the way. Companion to
[`modularity.md`](../architecture/modularity.md) (the seam/mesh plan) and
[`tui-shared-code-extraction-plan.md`](tui-shared-code-extraction-plan.md) (an earlier, narrower
extraction effort this audit builds on).

**Method**: Read every crate's `Cargo.toml` to build the internal dependency graph, then grepped
actual `use` sites in the crates flagged as "should be generic" (`executor`, `mcp`,
`conversation-store`, `provider`) to see exactly which types cross each edge — not just that an
edge exists.

**Status**: Findings recorded 2026-07-01. Items 1, 2, and 4 are **done** (verified with a full clean
`cargo build --workspace` + `cargo test --workspace`, zero failures); item 5 is **partially
resolved** (the effect the finding wanted is achieved, just not via its originally-proposed
mechanism — see item 5's own status note). **Item 3 — splitting `liberado-common` — is the sole
fully open item**, deferred pending a deliberate pass since it's the highest-effort,
most call-site-touching change of the five and benefits from the boundaries found while doing 4–5
(see "Recommended sequencing" at the bottom).

---

## What's already right (the bar is closer than it looks)

- **`chat-client-contract`** — zero internal workspace dependencies. Already passes the "could
  someone use just this crate?" test from `modularity.md`.
- **`liberado-provider` / `liberado-provider-deepseek`** — zero Liberado-specific coupling.
  `provider` depends on nothing internal; `provider-deepseek` depends only on `provider`. Exactly
  the shape a reusable inference layer should have.
- **`liberado-conversation-store`** — depends only on `provider` (for message/tool types), not on
  `liberado-common`. Already extractable, matching the claim already made in `modularity.md`.
- **`liberado-theme` / `liberado-markdown`** — standalone utility crates, zero internal deps.

So the core "coding-agent CLI" ingredients — provider abstraction, conversation persistence — are
already clean. The gaps are concentrated in `executor`, `mcp`, and the `common` grab-bag they both
lean on.

---

## 1. TUI duplicates the shared slash-command system (unfinished migration)

**Where**: `crates/tui/src/commands.rs` (353 lines) is a complete, hand-rolled slash-command
dispatcher (`dispatch(app: &mut App, input: &str) -> Vec<Effect>`) that reimplements exactly what
`liberado-commands` (the shared crate) already does — the crate the WebUI now correctly uses
(`crates/webui/src/components/slash_commands.rs`, wired up 2026-07-01).

**The unfinished part**: `crates/tui/src/command_context.rs` already contains
`impl CommandContext for App` — a real, seemingly-complete implementation of the shared trait — but
it is **not declared as a `mod` anywhere in `crates/tui/src/lib.rs`**. It is dead code: never
compiled, never wired to the TUI's actual input-handling path. Someone started this migration and
didn't finish it.

**Also missing**: `crates/tui/Cargo.toml` doesn't even depend on `liberado-commands` — confirming
`command_context.rs` can't currently build as part of the crate.

**Fix**: Finish the migration — route TUI input through `liberado_commands::dispatch` +
`command_context.rs`'s `CommandContext for App`, delete `crates/tui/src/commands.rs`'s duplicate
logic, add the `liberado-commands` dependency. Net effect: ~350 duplicate lines removed, and future
slash-command fixes (like today's) apply to both clients from one place.

**Status**: Done. `crates/tui/src/commands.rs` deleted (353 lines), `command_context.rs` wired
into `lib.rs`, `crates/tui/Cargo.toml` now depends on `liberado-commands`, and `App::handle_slash_command`
routes through `liberado_commands::parse`/`dispatch`. Bonus: this also fixed a real, previously
undetected bug — the old hand-rolled dispatcher used `splitn(2, ' ')`, so `/session switch <id>`
and `/theme set <name>` never correctly split subcommand from argument.

---

## 2. CLI and TUI each hand-roll an identical SSE decoder

**Where**: `crates/cli/src/chat_client.rs` (298 lines) and `crates/tui/src/sse.rs` (287 lines) each
independently define `SseEvent` / `SseDecoder` / `parse_block` / `strip_one_space` — same struct
names, same incremental string-framing algorithm, same edge cases covered by near-duplicate test
suites (multi-line `data:`, split-across-chunks, comment lines, CRLF).

**This is already tracked, just not finished**: `docs/roadmap/tui-shared-code-extraction-plan.md`
(written earlier) calls this out explicitly in its Step 4 — "Update \[the CLI\] to: Import
`SseDecoder` from `chat_client_contract::native::SseDecoder` instead of maintaining a private
copy... Delete the private `SseDecoder`, `SseEvent`, `parse_block`, and `strip_one_space`
definitions." That plan's Steps 0–3 (repointing the server/TUI/WebUI at shared wire types via
`chat_client_contract`, including the `ChatEvent::from_sse_data()` helper) are done — confirmed by
grep, `tui/src/sse.rs` already imports `chat_client_contract::ChatEvent` and uses
`from_sse_data()`. Only Step 4 (the decoder itself) is outstanding.

**One more orphan found along the way**: `chat_client_contract::native` already defines a
`ChatClient` trait (exactly the "one client trait for TUI/CLI" the plan wanted) — but **it is
implemented nowhere in the codebase**. Both `tui` and `cli` still use separate ad-hoc
`fetch_*`/`post_chat_stream`-style free functions instead. Second piece of half-finished seam
scaffolding, same shape as finding 1.

**Fix**: Move `SseEvent`/`SseDecoder`/`parse_block`/`strip_one_space` (plus their tests) into
`chat_client_contract::native`; have both `tui` and `cli` import from there and delete their local
copies. Leave the unimplemented `ChatClient` trait as a separate, smaller follow-up (adopting it
means restructuring both clients' transport functions around one trait shape — worth doing, but a
distinct, larger change from "move the decoder").

**Status**: Done — decoder moved to `chat_client_contract::native`; `tui/src/sse.rs` and
`cli/src/chat_client.rs` both import it now (`cli` alone dropped ~150 duplicate lines).
The unimplemented `ChatClient` trait was resolved 2026-07-05, not by adopting it — checked both
real clients first and found their actual transport needs diverge too much for one shared `send`/
`stream` trait to usefully capture (CLI: blocking terminal REPL; TUI: non-blocking render loop via
its own action/effect channels). Deleted the trait and documented `SseDecoder` (genuinely shared)
plus `ChatEvent::from_sse_data` (used by the TUI on top of it) as the real boundary instead — see
`hygiene-audit-2026-07-05.md` P2.5.

---

## 3. `liberado-common` is an unstructured nine-module grab-bag

**Where**: `crates/common/src/` holds `capability.rs`, `catalog.rs`, `config.rs`, `dispatch.rs`,
`error.rs`, `event.rs`, `model.rs`, `proposal.rs`, `provenance.rs` — nine largely-unrelated concerns
in one crate. Every crate that needs *any* one of them depends on *all* of them.

**Evidence of the cost** (grepped actual `use liberado_common::` sites, not just the `Cargo.toml`
edge):
- `liberado-executor` — the "MCP-agnostic agent loop" crate — imports exactly three names from the
  whole crate: `Outcome`, `Report`, `ToolCall` (`crates/executor/src/lib.rs:29`).
- `liberado-mcp`'s genuinely generic parts (`connector.rs`, `factory.rs`, `lib.rs`, `multi.rs`,
  `scoped.rs`) import only `WriteProvenance` and `mcp_of` — the loop-breaking provenance concept and
  a namespace-splitting helper.

Neither crate needs the capability/zone model, the config/TOML types, the proposal workflow types,
or the catalog types — but both drag all of it in as a compile-time and conceptual dependency.

**Why it matters for the stated bar**: lifting `provider` + `executor` + `mcp` +
`conversation-store` into a different, vault-agnostic product means dragging Liberado's entire
security/config/proposal type universe along as unused baggage — exactly the kind of "can't
actually use just this crate" failure `modularity.md`'s test is meant to catch.

**Fix (not yet started)**: split `common` along its existing module boundaries — e.g. a narrow
`liberado-agent-types` (`Outcome`/`Report`/`ToolCall`, what `executor` actually needs), a
`liberado-provenance` crate (`WriteProvenance`, loop-breaking, `mcp_of`), and leave
capability/config/proposal/catalog either together or further split. This is a workspace-wide
`Cargo.toml` + import-path change across most crates — real but mechanical once the boundaries are
chosen.

**Status**: Deferred — not started, awaiting a dedicated pass.

---

## 4. `liberado-mcp` depends sideways on `liberado-orchestrator`

**Where**: `crates/mcp/src/connector.rs` and `crates/mcp/src/factory.rs` import
`liberado_orchestrator::{RuntimeFactory, RuntimeSetupError}` — two traits that live in the
dispatch-decision-to-execution bridge crate, purely so `mcp` can implement a factory pattern for
building `ToolRuntime`s.

**Why it's backwards**: a crate whose job is "connect to real MCP servers, run tool calls" has no
conceptual need for `orchestrator` (which bridges `DispatchDecision` → execution). This is a
wrong-direction dependency: the low-level connector crate reaching *up and sideways* into the
higher-level dispatch-bridging crate.

**Fix (not yet started)**: move `RuntimeFactory`/`RuntimeSetupError` down into `liberado-executor`
(which already owns the `ToolRuntime` trait these factories build), or a new minimal crate. Cuts the
`mcp → orchestrator` edge entirely — `mcp` would then depend only on `provider` + the narrow
provenance piece from finding 3.

**Status**: Done (2026-07-02). Independently re-found by
[`hygiene-audit-2026-07-02.md`](hygiene-audit-2026-07-02.md)'s coupling pass (its item 6) after this
same edge nearly forced a circular dependency during that session's runtime-gating work — good
cross-confirmation this was real. `RuntimeFactory`/`RuntimeSetupError` moved to `liberado-executor`;
`liberado-mcp`'s dependency on `liberado-orchestrator` removed entirely (verified via
`cargo tree -p liberado-mcp` — zero occurrences).

---

## 5. `liberado-mcp` bundles the generic runtime with Liberado's safety-gating layer

**Where**: `crates/mcp/src/risk_gated.rs` (`RiskGatedToolRuntime`) pulls in `Capability`,
`ProposedAction`, and the full consequence/proposal vocabulary from `common` — Liberado's
capability-gating and proposal-downgrade machinery, applied as a wrapper around any `ToolRuntime`.

**Why it's a smell, not a bug**: the gating logic itself is legitimate, important, and correctly
factored as a wrapper (it doesn't leak into `TurbomcpRuntime`/`StdioConnector`/`HttpConnector`
directly). The problem is *where it lives* — bundling it into the same crate as the genuinely
generic connectors means the crate you'd want to reuse standalone (`liberado-mcp`) always carries
the full safety-gating type surface even when the consumer doesn't want or need Liberado's
capability model at all.

**Fix (not yet started)**: move `risk_gated.rs` into its own crate (or co-locate with wherever the
capability vocabulary lands after finding 3's split). `liberado-mcp` itself then only needs
`provider` + the narrow provenance crate.

**Status**: Partially resolved (2026-07-02) — `risk_gated.rs` moved out of `liberado-mcp` into
`liberado-executor` as part of that session's runtime-gating work (`RiskGatedToolRuntime` needed to be
reachable from `liberado-orchestrator`, which can't depend on `mcp`). Not "its own crate" as this
finding originally proposed, but the effect is the same: `mcp` no longer bundles the safety-gating type
surface. See [`hygiene-audit-2026-07-02.md`](hygiene-audit-2026-07-02.md) for the follow-on audit that
session's work prompted.

---

## Recommended sequencing

1. **Findings 1–2** (this doc's action items) — safe, mechanical, no architectural decisions
   required, net deletion of duplicate code. Execute now.
2. **Findings 4–5** — highest leverage for the stated "reusable coding-agent substrate" bar; together
   they make `provider + executor + mcp + conversation-store` a genuinely standalone, vault-agnostic
   four-crate set, not just an aspirational one. Both done as of 2026-07-02.
3. **Finding 3** (`common` split) — highest effort, touches the most call sites; do this once the
   shape of 4–5's extraction has proven out the pattern, so the module boundaries chosen for the
   split are informed by real reuse rather than guessed up front.
