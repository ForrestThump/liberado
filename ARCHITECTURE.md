# Liberado — Architecture

Liberado is a **Rust-native personal AI Life OS**: a daemon that watches your Obsidian vault, reasons
about changes with an LLM, and acts on your behalf through tools — safely, and without reacting to its
own work. This document is the cold-start map. Each crate has its own zoomed-in
`crates/<name>/ARCHITECTURE.md`.

## Two pillars

1. **The vault is the source of truth.** State lives as Obsidian Markdown, managed through Turbovault.
   The system perceives by watching the vault and acts by writing back to it. There is no separate
   database of record.
2. **Safety is engineered, not prompted.** The LLM *proposes*; deterministic code *disposes*, and only
   ever toward less autonomy. Capabilities only narrow, never widen. Provenance is best-effort and is
   never treated as a trust boundary — security is the capability/zone model.

## The loop (perceive → decide → act → don't loop)

```
            ┌─────────────────────────── vault (Obsidian Markdown, Turbovault) ───────────────────────────┐
            │                                                                                              │
            ▼                                                                                              │
  ┌──────────────────┐   external    ┌──────────────┐   decision   ┌───────────────┐   tool calls  ┌──────┴───────┐
  │  daemon: watch   │──── change ───▶│  dispatcher  │─────────────▶│ orchestrator  │──────────────▶│   executor   │
  │  debounce        │   (attributed │  classify +  │  Execute /   │ decision →    │   agent loop  │  + ToolRuntime│
  │  attribute ◀─────┼── as External)│   guards     │  Subagent /  │ Task + prov.  │               │  (liberado-  │
  │  (loop-break)    │               └──────────────┘   Clarify    └───────────────┘               │   mcp)       │
  └────────┬─────────┘                                                                              └──────┬───────┘
           │  Agent/Missing → SUPPRESS (our own write)                                                     │
           │                                                                                  writes carry │
           └──────────────── provenance in the audit log ◀──────── _meta provenance ◀─────────────────────┘
```

The dashed return path is the **loop-break** (Decision 5): an agent's tool call carries
`WriteProvenance` in the MCP request `_meta`; Turbovault records it on the write's audit-log entry; the
daemon's `attribute()` then recognizes the resulting vault change as *ours* and suppresses it instead
of reacting. Proven end-to-end in `crates/vault/tests/provenance_e2e.rs`.

## Crate map

Bottom-up (each depends roughly on those above it):

| Layer | Crate | Role |
|---|---|---|
| Types | [`common`](crates/common/ARCHITECTURE.md) | Shared vocabulary: provenance, capability, dispatch, event, model, config, proposal. No logic. |
| Inference | [`provider`](crates/provider/ARCHITECTURE.md) | The `Provider` narrow waist + `MockProvider`. No HTTP. |
| Inference | [`provider-deepseek`](crates/provider-deepseek/ARCHITECTURE.md) | Concrete DeepSeek backend (the only crate that talks to a model). |
| Vault | [`vault`](crates/vault/ARCHITECTURE.md) | Turbovault adapter: provenance writes + hash-join attribution (loop-breaking). |
| Decide | [`dispatcher`](crates/dispatcher/ARCHITECTURE.md) | classify (LLM) → guards (deterministic, downgrade-only) → `DispatchDecision`. |
| Act | [`executor`](crates/executor/ARCHITECTURE.md) | The agent loop: drive a `Provider` over a `ToolRuntime` to a `Report`. MCP-agnostic. |
| Act | [`mcp`](crates/mcp/ARCHITECTURE.md) | `TurbomcpRuntime`: the `ToolRuntime` over real MCP tools; injects provenance into `_meta`. |
| Act | [`orchestrator`](crates/orchestrator/ARCHITECTURE.md) | Bridges a `DispatchDecision` to an execution; chooses the provenance correlation. |
| Core | [`daemon`](crates/daemon/ARCHITECTURE.md) | The long-running watch→debounce→attribute→dispatch loop. |
| Root | [`cli`](crates/cli/ARCHITECTURE.md) | The `liberado` binary — the composition root. |

## Cross-cutting concepts

- **Provenance & loop-breaking (Decision 5)** — `WriteProvenance` (`source` + `correlation_id`) rides
  the audit log, not frontmatter. Consumers attribute by content identity (hash-join), not timing.
- **Capability/zone containment (Decision 4)** — `CapabilitySet` is narrow-only; a subagent gets
  `base ∩ narrowing`. This is the actual security boundary.
- **Provider-agnostic inference (Decision 13)** — one `Provider` trait, swappable from config, with
  role-tiered model floors. Tests inject `MockProvider` (Decision 16).
- **MCPs vs ACPs** — MCPs are **tools** the agent *calls* (work). ACPs are **event sources** that
  *push* into the daemon (the `Event` type serves both trigger paths). Today only the vault watcher
  produces events.
- **Daemon-first (Decision 2)** — one long-running process; the CLI/TUI attach to it.

## Co-development with Turbovault & Turbomcp

`turbovault/` and `turbomcp/` are **sibling repos**, consumed as path dependencies during
co-development (Decision 7) and excluded from this workspace. A root `[patch.crates-io]` redirects
Turbovault's published `turbomcp` to the local fork so the whole tree builds against one Turbomcp —
the one carrying the request-`_meta` pass-through that the provenance loop depends on. (Those upstream
changes live on feature branches and have a draft issue in `turbomcp-request-meta-issue-draft.md`.)

## Current status

Every box above exists, is **independently tested**, and is now **wired end-to-end**: the daemon
watches → attributes → dispatches → orchestrates → executes, with both wiring seams complete.

1. ✅ **Concrete `RuntimeFactory`** — `liberado-mcp`'s `TurbomcpRuntimeFactory` connects via a
   `ClientConnector` (production: spawn an MCP server subprocess over stdio), builds a
   provenance-bound `TurbomcpRuntime`, and scopes it to the allowed MCPs.
2. ✅ **Daemon → orchestrator** — the daemon's `react()` runs dispatch → orchestrate; a `Reaction`
   carries a `ReactionOutcome` (`Observed` / `Decided` / `Acted(Disposition)`). The `cli` assembles
   a `StdioConnector`-backed orchestrator when `LIBERADO_MCP_CMD` is configured.

What's deliberately *not* done yet (future slices, not seams): connection **pooling/reuse** (today
one connection per execution) and a **multi-server** MCP registry (today one server per connector);
the dispatcher's classifier prompt **hardening** (delegation bias); ACP event sources beyond the
vault watcher; proposal production for high-consequence writes (Decision 11).

## Where to start reading

1. This file.
2. [`common`](crates/common/ARCHITECTURE.md) — the vocabulary everything speaks.
3. [`vault`](crates/vault/ARCHITECTURE.md) — attribution / loop-breaking (the conceptual heart).
4. [`dispatcher`](crates/dispatcher/ARCHITECTURE.md) → [`orchestrator`](crates/orchestrator/ARCHITECTURE.md)
   → [`executor`](crates/executor/ARCHITECTURE.md) — the decide→act path.
5. [`daemon`](crates/daemon/ARCHITECTURE.md) — how it all runs.

The deeper "why" behind each Decision N lives in the root planning docs (`*-spec.md`,
`life-os-architecture.md`).
