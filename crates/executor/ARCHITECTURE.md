# liberado-executor — the agent loop engine

A bounded, adaptive tool loop. Given a goal and a `ToolRuntime` (the tools it may call and how to run
them), it drives a `Provider` turn by turn — the model proposes calls, we run them, feed the results
back, and let it decide the next step — until the task terminates.

## The organizing principle: termination follows the consumer

There are two public entry points sharing one private loop, chosen by **who reads the output**:

| Method | Consumer | Termination | Returns |
|---|---|---|---|
| `execute()` | another agent (delegated work) | model calls the synthetic **`submit_report`** finish-tool | typed `Report` |
| `converse()` | a human | model replies with prose and no tool call | `String` |

The finish-tool's parameter schema *is* the `Report` schema, so filing both **terminates** the loop
and **hands back** the structured artifact in one event — no second "structuring" call.

## Surface

- `ToolRuntime` trait — `catalog() -> Vec<ToolDef>` (sync; pre-fetched) and
  `async invoke(&ToolInvocation) -> Result<String, String>`. Tool-level failures are returned `Err`
  and surfaced to the model **in-band** so it adapts; only infra faults abort.
- `Executor` (holds `Arc<dyn Provider>` + `Budget`), `Task` (`instructions`/`goal`/optional
  `seed_calls`), `Budget`, `ExecError`, `SUBMIT_REPORT_TOOL`.

## Backstops

- **Turn `Budget`** — a hard cap; exhaustion becomes a `Failed` `Report` (the delegator is owed a
  report, not a transport error).
- **Single nudge** — in report mode, if the model answers in prose without filing, it's nudged once,
  then the prose is wrapped as a `Report` rather than lost.
- **Seed calls** — `ExecuteDirect`'s opening move is executed as a synthetic first turn, then the
  loop continues adaptively (the field is a *seed*, not a fixed plan).

## Dependencies

- Depends on: `liberado-common` (`Report`/`Outcome`/`ToolCall`), `liberado-provider`.
- Depended on by: `mcp` (implements `ToolRuntime`), `orchestrator` (drives `execute()`).
  This crate is **MCP-agnostic** — it knows the `ToolRuntime` trait, not turbomcp.

## Tests

Inline: multi-turn→file, conversational multi-tool→prose, budget→failed report, malformed
`submit_report` args→decode error, seed-before-first-turn, nudge-then-wrap, in-band tool failure.
