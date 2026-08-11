# What to take from pi, Hermes and Deep Agents

**Status**: Research + proposal, 2026-08-11. No code. Read alongside
[`coder-harness-reliability-2026-08.md`](coder-harness-reliability-2026-08.md), which has the
measurements this builds on.

**Why these three.** A third-party benchmark ranks them the most cost-efficient harnesses when run
on `deepseek-v4-flash` — the model class we actually use. That claim is second-hand and unverified
here; what follows is a read of their source, which is checkable. Checkouts live at `pi/`,
`hermes-agent/`, `deepagents/` (gitignored). **All three are MIT**, so forking is unencumbered.

---

## The one-paragraph version

Their cost advantage is not prompt wording. It is **structural: they keep tokens out of the context
window in the first place.** Deep Agents writes oversized tool results to disk and hands the model a
path. Hermes lets the model write a script that performs many tool operations and returns one
result. pi carries structured file-operation lists across compaction instead of re-reading. All
three treat context management as a *replaceable strategy* rather than fixed code — which is also
the architectural precondition for the per-model tuning system in
[`model-knob-profiles.md`](model-knob-profiles.md).

---

## Cost levers, ranked by leverage for us

### 1. Offload oversized tool results to disk — Deep Agents

`middleware/_message_eviction.py`. When a tool result exceeds a threshold the middleware writes it
to the filesystem and replaces it in-context with a head+tail preview plus:

> *"Tool result too large, the result of this tool call {id} was saved in the filesystem at this
> path: {path}. You can read the result from the filesystem by using the read_file tool, but make
> sure to only read part of the result at a time."*

**Why this is our biggest lever.** We currently *truncate* — `TRACE_MAX_CHARS`, `read_max_bytes`,
`output_max_bytes`. Truncation and offload cost the same context; only offload keeps the data
reachable. Our worst offender is `run_command`: a failing `cargo test --workspace` is tens of
kilobytes, it lands in context in full or clipped to uselessness, and it **stays there for every
subsequent turn**. Given the measured finding that 56% of all spend is re-sent base context, moving
build output out of the window is the single highest-value change on this list.

It also fixes a correctness problem we already hit: a 120s command timeout once returned *no output
at all*, and a 500-character clip dropped the part of a compiler error that explained the run.

### 2. Script-over-RPC: many tool calls, one context entry — Hermes

Hermes's pitch: *"Write Python scripts that call tools via RPC, collapsing multi-step pipelines into
zero-context-cost turns."*

Every tool call in a normal loop costs a full round trip — the entire conversation is re-sent to get
one result back. Ten reads is ten re-sends. A script that performs ten reads and returns one summary
is **one** re-send. For exploration-heavy work, which is exactly what the reads-per-edit measurements
show our runs doing, this is a large multiplier.

Bigger and riskier than #1: it needs a sandboxed interpreter with an RPC channel back to the tool
runtime, and a command policy that survives contact with arbitrary scripts. Worth scoping only after
#1 lands.

### 3. Prompt caching as an explicit, provider-aware layer — Deep Agents and pi

Deep Agents has `_prompt_caching.py` with per-provider middleware (Anthropic, Bedrock) that degrades
to a no-op when the provider is unsupported. pi sets `cache_control` with an explicit TTL and reads
`cacheWrite1h` back out of usage.

We got the *ordering* right in TE3 (stable content before the varying goal) but we do not set cache
directives on the coding path, and we do not read cache-hit figures back per run. Ordering without
directives leaves provider-side caching to luck, and without the read-back we cannot tell whether it
worked — which is the same "no instrument" problem the trace gap was.

### 4. Compaction that preserves structured file operations — pi

`harness/compaction/compaction.ts` threads `readFiles` and `modifiedFiles` through each compaction
as **data**, not prose. A summarised history therefore still knows exactly which files matter, and
the model does not re-read them to find out.

Our compaction (CH3) summarises to text. Carrying a file-op list across the boundary is a small,
self-contained improvement with an obvious payoff for coding sessions.

---

## Reliability levers

### 5. Classify tools by side effect — Hermes

`tool_result_classification.py` keeps a `NO_EFFECT_TOOL_NAMES` set (reads, searches, snapshots) and
treats everything else — including unknown MCP and plugin tools — as effect-capable by default. An
interrupted no-effect call is safe to discard; an interrupted mutation is not.

We need this for cancel and resume, where we currently have no principled answer to "was that call
safe to drop". The default-deny posture for unknown tools is the right shape and matches our
capability model.

### 6. Prove the mutation landed — Hermes

`file_mutation_result_landed()` parses a write's result and confirms `bytes_written` (or
`success: true` for a patch) before believing it. **This would have caught PR #106**, where
`write_file` silently destroyed a file and reported success. Cheap, deterministic, and it closes a
class we have already been burned by.

### 7. Middleware composition — Deep Agents

Every concern is a separate, individually configurable unit: `filesystem`, `summarization`,
`subagents`, `permissions`, `skills`, `rubric`, `patch_tool_calls`, `_overflow_clip`,
`_prompt_caching`. Adding or removing one is configuration, not surgery.

This is the architectural enabler for per-model tuning. A knob that requires a recompile is not a
knob, and a monolithic loop cannot expose many of them. Our `executor` loop is closer to pi's shape
than to this; the guards are inlined into `run_loop` rather than composed, which is why the
doom-loop fix in #128 had to reach into the middle of the loop.

### 8. Telemetry as a contract, with conformance tests — pi

`@earendil-works/pi-telemetry` is deliberately vendor-neutral: an explicit `TelemetryContext` passed
by argument, no global current-span state, no exporter, and a **conformance test suite any adapter
must pass**. Domain schemas are declared separately and typed.

We have traces (#117, #124) and they have already paid for themselves, but they are a coding-pack
concept (`CoderEvent`) rather than a contract. If the tuning ledger below is going to ingest runs
from four different harnesses, the event contract has to come first and be adapter-shaped.

---

## Forking all three for comparable logging

The A/B against Kilo Code produced real findings, but comparing two harnesses required writing a
translation layer for their event formats, and the metric was only trustworthy because one script
categorised both sides. That does not scale to four.

**Proposal: fork pi, Hermes and Deep Agents, and add one common trace emitter to each.** MIT
licences make this clean. Each fork emits the same event envelope we already write —
`model_request_sent`, `tool_started`/`tool_finished` with an `ok` flag, `session_finished` /
`session_aborted` — so a single parser scores all four.

Then run **A/B/C/D**: one task suite, one model, four harnesses, identical prompts. What to record
per run:

| Field | Why |
|---|---|
| tokens in / out / cached | the cost claim, tested rather than repeated |
| wall clock, turns used | latency and budget pressure |
| edits, edit failures | the reliability number we already track |
| reads per successful edit | task-shape indicator, **not** a quality score — see below |
| tools withdrawn | the failure that cost us the last A/B |
| terminal outcome + gates | the only bar that matters: does it compile and pass |

**A caution earned the hard way.** Reads-per-edit looked like the discriminator after the first
Kilo A/B (6.5 versus our 1.0). On the next task Kilo scored 1.0 and still shipped a clean pass. The
metric tracks *task shape*, not harness quality. Any dashboard built on it will mislead. Score on
the gates; use the rest to explain, never to rank.

---

## What not to copy

- **Deep Agents' framework depth.** It sits on LangChain and LangGraph. The ideas are portable; the
  dependency is not, and dependency sovereignty is the point of this project.
- **Hermes's breadth.** Seven terminal backends, browser automation, transcription, billing views.
  Interesting, unrelated.
- **pi's self-extension.** An agent that rewrites its own harness is the opposite of what a
  measurement programme needs — the instrument must hold still.

---

## Order of work

1. **Tool-output offload to disk** (#1). Largest measured payoff, self-contained, and it fixes a
   truncation problem we already have.
2. **Mutation landed check** (#6) and **side-effect classification** (#5). Both small; #6 closes a
   class we shipped a bug in.
3. **Cache directives plus cache-hit read-back** (#3). Cheap, and it makes TE3 verifiable.
4. **The event contract** (#8) — prerequisite for the ledger and for A/B/C/D.
5. **Fork the three and instrument them.** Only after #4, so there is one schema to target.
6. **Script-over-RPC** (#2). Biggest lever, biggest blast radius. Last.

Compaction file-op lists (#4) and middleware decomposition (#7) are opportunistic — do them when
touching those areas anyway.
