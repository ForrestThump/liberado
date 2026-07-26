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
| every `allowed_mcps` entry below `CONSEQUENCE_GATE` | if something happened *out in the world*, only the main agent can re-dispatch or explain a partial action |
| `Outcome::Succeeded` | a failure or partial belongs in the conversation, not filed as a finished document |
| path names a zone, no `..`, not absolute | it is a model-produced path addressing a write |
| zone is `allows_direct_agent_write()` | an undeclared zone defaults to `ProposalOnly` — a hallucinated destination is refused, not created |
| pool holds `Write(Zone::vault(zone))` | the orchestrator writes under its own authority here |
| the report **looks like a document** (`looks_like_a_document`) | the subagent declares its own success and nothing else looks |

Row 1 is deliberately **not** `is_read_only_dispatch`. Those were the same predicate until a live run
showed why they must not be: "research X and save it to my vault" is the clearest case for direct
delivery, and it is exactly the phrasing that makes a classifier reach for the vault MCP — which is
`Reversible`, so the dispatch stopped being read-only and delivery refused. The feature switched
itself off precisely when asked for most plainly. What delivery cares about is narrower than "did
this write anything": *did something happen the main agent must narrate?* A git-revertable vault
write is not that. `is_read_only_dispatch` still governs `salvageable`, where the question really is
"could this have left something half-written" — the two are *supposed* to disagree on a vault-reading
dispatch, and a test pins that so they are not re-fused.

Rows 4–5 exist because `deliver_to_vault` deliberately skips `gate()` — a `RiskGatedToolRuntime`
would turn a restricted zone into a *proposal*, and filing a research note should be one silent
write or nothing. Skipping the gate must not mean skipping its rules, or this becomes an unguarded
write path into the vault (the F1 shape: a guard absent because a new code path grew around it).

Row 6 is the mechanical half of a guarantee the prompt already makes. `delivery_directive` tells the
subagent its report *is* the document — needed because `Report::summary` is contractually "short"
everywhere else, so without it the subagent writes a status line and waits to author the real thing
with a tool delivery deliberately withheld. A live run filed 231 bytes reading *"I have all the
research I need. Let me now write the comprehensive report directly to the vault."* A prompt holds
while the model complies and the prompt is unedited; a length-and-structure assertion holds
regardless. An LLM grading its own output would add nothing.

Because the directive must be given *before* the run and the outcome is only known *after*,
`delivery_target` is the outcome-independent half of the decision, shared by both callers. A dispatch
that will **not** deliver deliberately does not receive the contract — otherwise a downgraded run
emits a 20KB "summary" into the chat it was meant to keep short.

### Depth (`Depth`)

`Depth` (`Shallow`/`Normal`/`Deep`) selects the turn budget via `budget_for`, capped by the pool's
research ceiling. Declared by the dispatch rather than derived: budget, loop profile, and delivery
were all inferred from one read-only predicate, and they are three different questions. A
deep-research goal that merely mentioned the vault got 8 turns instead of 30 and failed at the
ceiling. `salvageable` remains inferred — from consequence, not depth — because it is a safety
property rather than a preference.

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
