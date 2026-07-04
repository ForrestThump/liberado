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

## Known limitation: multi-step tool chaining is not fully reliable

Live tuning of the executor/subagent prompts (`liberado-heuristics-tuner`) found that a genuinely
simple two-tool-call goal ("research via one tool, write via another") fails to reach a clean
`Succeeded` report a large fraction of the time against `deepseek/deepseek-v4-flash`, even under a
system prompt that explicitly instructs the exact sequence needed. One real contributing bug was
found and fixed here — `REPORT_NUDGE` used to unconditionally push toward `submit_report` the first
time a model paused in prose, with no "keep going" option, competing against whatever the system
prompt said at exactly the moment a model paused mid-plan. The fix (reworded to offer both options)
measurably helped but did not fully resolve the gap. This is a project-level open finding, not just a
tuner curiosity — see [`docs/roadmap/multi-step-execution-reliability-finding.md`](../../docs/roadmap/multi-step-execution-reliability-finding.md)
for the full evidence, what's ruled out, and what's still open.
