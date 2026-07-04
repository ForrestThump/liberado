# liberado-orchestrator — the decide→do bridge

Turns a dispatcher `DispatchDecision` into an actual execution. It is the seam between *deciding*
(dispatcher) and *doing* (executor + a tool runtime).

## `Orchestrator::run(decision, goal, trigger_correlation) -> Disposition`

| Decision | What it does | Provenance correlation |
|---|---|---|
| `Clarify` | returns `Disposition::Clarify` — **nothing executes** | — |
| `ExecuteDirect` | builds a `Task` (tight `Budget(4)`) seeded with the opening calls, runs the executor loop → `Reported(Report)` | the **triggering** event's correlation (it acts in the reaction's name) |
| `DispatchSubagent` | narrows the runtime to `allowed_mcps`, builds instructions from `success_criteria`, larger budget → `Reported(Report)` | the action's **own** `correlation_id` |

`Disposition` = `Reported(Report)` \| `Clarify { questions, what_blocked }`.

## The key decoupling: `RuntimeFactory`

The orchestrator does **not** know how to connect to MCP servers. It depends on a trait:

```rust
async fn runtime_for(&self, allowed_mcps: &[String], provenance: WriteProvenance)
    -> Result<Box<dyn ToolRuntime>, RuntimeSetupError>;
```

The real implementation (turbomcp-backed, connection management) lives in the MCP layer and is a
separate slice; this crate is fully unit-tested with a mock factory + `MockProvider`. The factory is
where the per-execution `provenance` (built here from the right correlation) is handed to the runtime
that will inject it into `_meta`.

## Why the correlation split matters

`ExecuteDirect` adopts the triggering correlation so its writes are attributed to *this reaction*;
a `DispatchSubagent` uses the classifier-minted id so the subagent's writes (and any cascade) are
traced to *that* goal. Both ride the loop-breaking + idempotency machinery.

## Dependencies

- Depends on: `liberado-common` (decision/report types), `liberado-provider` (`Arc<dyn Provider>`),
  `liberado-executor` (`Executor`/`Task`/`ToolRuntime`).
- Depended on by: `bootstrap` (composition), `daemon`, `main-agent`, `server` — not `cli` directly,
  which only depends on `liberado-server`. Also `heuristics-tuner` (reads `DIRECT_INSTRUCTIONS`/
  `SUBAGENT_PREAMBLE`/`DIRECT_MAX_TURNS` as seed values for executor/subagent prompt tuning, without
  going through `Orchestrator` itself) and `test-support` (shared test doubles).

## Tests

`tests/orchestrate.rs`: `ExecuteDirect` adopts the trigger correlation; `DispatchSubagent` uses its
own correlation + `allowed_mcps`; `Clarify` short-circuits (no provider call, no runtime built).
