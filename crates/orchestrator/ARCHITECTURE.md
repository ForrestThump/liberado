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

### Report delivery (`Delivery`)

A `DispatchSubagent` carries a `delivery` saying where its terminal `Report` goes. Default
`Summarize` — back to the main agent, which narrates it. `Vault { path }` instead makes the
**orchestrator** file the body itself: one deterministic tool call through the configured
`ReportSink`, no model reading the body on the way, and the main agent gets back a *receipt* (path
+ size) it has nothing to restate from. That second re-emission was the cost being removed — decode
is sequential and latency-dominant, while ingesting the body on a later turn is near-free.

Delivery is model-chosen, so it is checked afterwards, and every check can only **downgrade** to
`Summarize` — never fail the run, never upgrade:

| Check | Why |
|---|---|
| every `allowed_mcps` entry is `ReadOnly` | if the subagent could *act*, only the main agent can re-dispatch or explain a half-done action |
| `Outcome::Succeeded` | a failure or partial belongs in the conversation, not filed as a finished document |
| path names a zone, no `..`, not absolute | it is a model-produced path addressing a write |
| zone is `allows_direct_agent_write()` | an undeclared zone defaults to `ProposalOnly` — a hallucinated destination is refused, not created |
| pool holds `Write(Zone::vault(zone))` | the orchestrator writes under its own authority here |

The last two exist because `deliver_to_vault` deliberately skips `gate()` — a `RiskGatedToolRuntime`
would turn a restricted zone into a *proposal*, and filing a research note should be one silent
write or nothing. Skipping the gate must not mean skipping its rules, or this becomes an unguarded
write path into the vault (the F1 shape: a guard absent because a new code path grew around it).

The sink (`[topology.report_sink]`) is declared, not inferred — this crate is kernel-layer and
reaches the vault only as an MCP tool call, so it must be told the tool and argument names.
`validate_merged_config` refuses to boot on a sink naming a missing, disabled, read-only, or
non-writing tool; with no sink declared, `Vault` simply downgrades and nothing changes.

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
