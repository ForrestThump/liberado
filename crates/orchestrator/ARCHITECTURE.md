# liberado-orchestrator — the decide→do bridge

Turns a dispatcher `DispatchDecision` into an actual execution. It is the seam between *deciding*
(dispatcher) and *doing* (executor + a tool runtime).

## `Orchestrator::run(decision, goal, trigger_correlation) -> Disposition`

| Decision | What it does | Provenance correlation |
|---|---|---|
| `Clarify` | returns `Disposition::Clarify` — **nothing executes** | — |
| `ExecuteDirect` | builds a `Task` (tight `Budget(4)`) seeded with the opening calls, runs the executor loop → `Reported(Report)` | the **triggering** event's correlation (it acts in the reaction's name) |
| `DispatchSubagent` | scopes runtime to `allowed_mcps`; risk-gates with `ceiling ∩ allowed_mcps` when decision `capabilities` is empty (classifier never emits capability objects); larger budget → `Reported(Report)` | the action's **own** `correlation_id` |
| `Propose` | builds a signed proposal in memory (caller persists) | — |

`Disposition` = `Reported(Report)` \| `Clarify { questions, what_blocked }` \| `Propose(SignedProposal)`.

### Out-of-band deferral flag (suppresses a redundant chat reply)

When the risk gate defers a call to the human mid-run (a proposal downgrade or a permission request)
**and** the notification actually sent, `RiskGatedToolRuntime` raises a shared flag that `run()`
reads back after `execute()` and stamps onto `Report::deferred_to_human`. `Disposition::deferred_to_human()`
surfaces it. Downstream, the dispatch pack folds it into `GoalResult.diagnostics`, and the face
agent's `delegate` reads that to collapse its now-redundant "you need to grant permission" reply to a
tiny marker — the out-of-band Telegram notification becomes the sole communication. The flag stays
`false` with no notifier or a failed send, so the chat reply remains the fallback. See the module
docs in `risk_gated.rs` and `main-agent`'s `face.rs`/`sessions.rs`.

**Not** full inherit of every dispatcher tool: subagent authority is always a narrow intersection
with the pool/dispatcher ceiling (Decision 4). Empty `allowed_mcps` means all MCPs under the ceiling
for runtime factory + gate (same sense as empty `relevant_mcps` on direct).

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
