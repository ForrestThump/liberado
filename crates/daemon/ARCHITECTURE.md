# liberado-daemon — the long-running core

The always-on heart of Liberado (Decision 2: daemon-first). It watches the vault and turns external
changes into routed reactions. The reactive decision is split into a **pure, deterministic core**
(`process_change`, testable without the filesystem) and the watcher plumbing (`run`).

## Pipeline

```
vault watch ─► debounce ─► attribute ─► dispatch ─► orchestrate ─► Reaction
 (notify)      (per-path,   (vault crate  (optional   (optional       { event,
               coalesce a   hash-join:    Dispatcher)  Orchestrator)    outcome }
               notify burst)Agent/External)
```

1. **watch** — Turbovault's notify-based watcher (own-watcher fallback, concurrency spec §8.1).
2. **debounce** (`debounce.rs`) — a pure, clock-injectable `Debouncer` coalesces the Create+Modify
   burst Windows/`notify` fires per write into one settled change (`DEFAULT_DEBOUNCE = 400ms`).
3. **attribute** — `process_change` calls the vault's `attribute()`. `Agent`/`Missing` → suppressed
   (our own write, or vanished). `External` → build a standardized `Event` and react.
4. **dispatch → orchestrate** (`react`) — takes the reaction as far as the attached components allow.

The loop is a `tokio::select!` over `watch.next_event()` vs. the next debounce deadline.

## Reaction outcomes (how far it got)

`react()` degrades gracefully; the `ReactionOutcome` records the stage reached:

| Attached | Outcome | Meaning |
|---|---|---|
| nothing | `Observed` | watch-only; the change is surfaced, not routed |
| dispatcher | `Decided(DispatchDecision)` | routed to a decision, but nothing to execute it |
| dispatcher + orchestrator | `Acted(Disposition)` | decided **and executed** (a `Report`, or a surfaced `Clarify`) |

Failures at any stage are logged and *degrade* the outcome (e.g. an orchestration error falls back to
`Decided`); they never abort the watch loop.

## Surface

- `Daemon::open` / `with_debounce` / `with_dispatcher` / `with_orchestrator` (builders) / `run`.
- `process_change(rel_path) -> Option<Event>` — the unit-testable attribution core.
- `Reaction { event, outcome: ReactionOutcome }`, `VAULT_NOTE_CHANGED`.
- `build_event` mints the correlation id `vault-change:<rel>:<short-hash>` keyed on path + content;
  this is the correlation an `ExecuteDirect` adopts as its write provenance.

## Dependencies

- Depends on: `liberado-common`, `liberado-vault` (attribution), `liberado-dispatcher`,
  `liberado-orchestrator` (execution).
- Depended on by: `cli`.

## Tests

Inline: debouncer behavior; `process_change` suppress-vs-react; daemon routes a reaction through a
mock dispatcher; correlation-id shape.
