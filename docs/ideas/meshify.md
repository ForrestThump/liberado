# meshify-idea.md — Loosening Liberado into a True Mesh

> **[audit note, 2026-07-11]** Partially superseded — keep for history, but read with these
> corrections:
> - "The Problem Today" is stale: the hard-coded flow is gone. `EventSource` (vault-watch + cron),
>   `POST /api/hooks/{name}`, and named dispatcher/executor pools (`[[pools]]` in `topology.toml`)
>   all landed — steps 1–4 here happened in spirit, via traits + config rather than a broadcast bus.
> - Step 5's "no component holds a direct pointer to another" was deliberately **not** adopted: the
>   agent-pools research (2026-07, four passes) found peer-agent coordination unproven; pools never
>   talk to each other, and the runtime is a hub around one daemon, not a peer mesh.
> - The live capability catalog (step 3) landed as the shared `Arc<CapabilityCatalog>`.
> - Verdict on the "mesh" framing overall:
>   [`architecture-alignment-audit-2026-07-11.md`](../roadmap/architecture-alignment-audit-2026-07-11.md).

**Goal**: Turn the current tight pipeline (watch → dispatch → execute) into a set of independent, swappable services that talk through events instead of direct calls.

This makes the system easier to extend, test, run partially, or move to other machines later — while keeping every safety guarantee intact.

---

## The Problem Today

Right now the flow is hard-coded:

```
daemon → dispatcher → orchestrator → executor
```

Every piece knows about the next one. Adding cron, a second dispatcher, or a remote executor means touching multiple crates.

---

## The Mesh Vision (in plain words)

Think of Liberado as a small internal message board.

- The daemon posts “vault changed” notes.
- The dispatcher, cron, and chat services each watch the board and post their own decisions.
- The executor pool watches for “please run this” notes and posts results back.

No component holds a direct pointer to another. You can unplug or duplicate any service without breaking the rest.

---

## How to Get There (Step by Step)

### 1. One Event Bus Inside the Daemon
- Add a tiny async channel (`tokio::sync::broadcast` or `flume`).
- Every major crate only knows how to **post to the bus** and **listen on the bus**.
- Direct function calls between crates disappear.

### 2. Declare Services in Config
Add a new optional file or section (`services.toml` or inside `topology.toml`):

```toml
[[service]]
name = "dispatcher-deepseek"
kind = "dispatcher"
enabled = true

[[service]]
name = "executor-local"
kind = "executor"
enabled = true
max_concurrency = 2

[[service]]
name = "cron"
kind = "scheduler"
enabled = false          # turn on when ready
```

Only enabled services start. Missing ones are simply absent — zero code changes elsewhere.

### 3. Live Capability Catalog
Instead of a static list, let every MCP server register itself at runtime. The dispatcher asks the catalog “what tools exist right now?” on every decision. This is the same registry the web UI and TUI will query.

### 4. Multiple Dispatchers & Executors at Once
Want a cautious dispatcher + a fast one? Two executor pools (local + Docker)?  
Just declare two `[[service]]` blocks of the same kind. The bus fans the work out; results merge by correlation ID.

### 5. Safety Stays in the Bus Layer
All narrowing, zone checks, provenance stamping, and magnitude gates live on the bus. Individual services never get to widen capabilities — they only consume or produce events the bus has already validated.

---

## What This Unlocks Immediately

- **Cron / scheduler** becomes just another listener on the bus.
- **MCP-proposal + coding-agent** pattern: the proposal service posts an event, a dedicated coding executor (spawned via the bus) builds the MCP and reloads the catalog.
- **Partial deploys**: run only the watcher + executor on a cheap VPS; run the dispatcher on a GPU box.
- **Testing**: swap in a mock dispatcher or mock executor with one line in the config.

---

## Migration Path (Zero Big Bang)

1. Keep the existing crates.
2. Wrap the current direct calls behind the new `EventBus` trait.
3. Gradually move internal logic to publish/subscribe.
4. The public HTTP/SSE API and the TUI client never change.

---

## One-Sentence Summary

Replace “A calls B” with “A posts an event that any interested B can consume” — and declare the Bs in a config file instead of hard-coding them.

That single change makes Liberado a true, extensible mesh while preserving every safety property it already has.