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

- **Turn `Budget`** — a hard cap; exhaustion becomes a `Report` — `PartiallySucceeded` (with a
  compact "tool → result preview" summary) if any call actually succeeded before time ran out,
  `Failed` only if none did (`budget_failed_report_with_progress`) — never a transport error, since
  the delegator is owed a report either way.
- **Single nudge** — in report mode, if the model answers in prose without filing, it's nudged once,
  then the prose is wrapped as a `Report` rather than lost (`REPORT_NUDGE`).
- **Doom-loop guard** — detects a model stuck repeating the same tool with near-duplicate arguments
  (`is_doom_loop`, TF-IDF cosine similarity over the arguments — not byte equality, which a model can
  defeat just by rewording the same question) or cycling between a short, fixed sequence of tools
  (`detect_short_cycle`, e.g. A,B,A,B). Escalates rather than immediately failing: nudge once, then
  actually remove the offending tool(s) from what's callable for the rest of the task (with a
  one-time, bounded turn-budget top-up — `DOOM_LOOP_RECOVERY_BONUS_TURNS` — since removal arriving on
  the very last turn can never pay off otherwise), then give up honestly if it still persists. See
  `docs/roadmap/multi-step-execution-reliability-finding.md`'s "Follow-up session" for the live
  evidence (real models get stuck this way; a nudge alone doesn't reliably redirect them) and why
  each step is shaped the way it is.
- **Seed calls** — `ExecuteDirect`'s opening move is executed as a synthetic first turn, then the
  loop continues adaptively (the field is a *seed*, not a fixed plan).

## Dependencies

- Depends on: `liberado-common` (`Report`/`Outcome`/`ToolCall`), `liberado-provider`.
- Depended on by: `mcp` (implements `ToolRuntime`), `orchestrator` (drives `execute()`).
  This crate is **MCP-agnostic** — it knows the `ToolRuntime` trait, not turbomcp.

## Tests

Inline: multi-turn→file, conversational multi-tool→prose, budget→failed/partially-succeeded report
(with and without real progress), malformed `submit_report` args→decode error, seed-before-first-turn,
nudge-then-wrap, in-band tool failure, doom-loop detection (near-duplicate args, single-turn batched
duplicates, false-positive avoidance for genuinely distinct same-tool calls), short-cycle detection,
each escalation step (nudge/removal/give-up) for both guards, and the tight-budget recovery-bonus
regression.

## Multi-step tool chaining: substantially fixed, one known gap remains

Live tuning of the executor/subagent prompts (`liberado-heuristics-tuner`) originally found that a
genuinely simple two-tool-call goal ("research via one tool, write via another") failed to reach a
clean `Succeeded` report a large fraction of the time against `deepseek/deepseek-v4-flash`, even under
a system prompt that explicitly instructed the exact sequence needed. A first contributing bug
(`REPORT_NUDGE` unconditionally pushing toward `submit_report`) was found and fixed, but the core gap
remained open. A follow-up investigation found the actual root cause — DeepSeek and Gemini both got
stuck repeating the same tool call with reworded-but-same-intent arguments — and closed most of it
with the doom-loop guard described above, live-verified going from 0/6 to 5/6 on the original failing
scenario across two models. Full narrative, evidence, and the one remaining open gap (a fast-finish
timing case, not a loop) in
[`docs/roadmap/multi-step-execution-reliability-finding.md`](../../docs/roadmap/multi-step-execution-reliability-finding.md).
