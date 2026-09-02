---
kind: plan
status: active
authority: implementation
domain: coding-harness
canonical_for: cross-harness-baseline
open_items: true
---

# Cross-harness baseline — backlog 0.7 / C3

**Status**: experiment spec. The comparison runner exists. The published four-way score does not.
**Selectable work**: yes. This is backlog item 1.
**This is a report**, not a Liberado coding-loop change.

The runner contract lives in
[`harness-comparisons.md`](../spec/reference/harness-comparisons.md).
The cost-lever research that this measurement must inform lives in
[`harness-study-2026-08.md`](harness-study-2026-08.md).
Earlier one-task A/Bs are evidence, not this baseline; see
[`coder-harness-reliability-2026-08.md`](coder-harness-reliability-2026-08.md).

## 1. What this item is

Pin Liberado, Pi, Hermes, and Deep Agents. Run them on one frozen task, one repository commit, one
model/provider pair, and fixed sampling and resource limits. Keep each harness's native system
prompt and tool schemas. Repeat the pairing. Publish a dated evidence record.

The product question is: on the model class Liberado actually uses, under a fixed budget, what
ship-gate rate, merge-ready rate, and cost per accepted result does each harness produce?

Two follow-on questions ride the same table:

1. Did tool-output offload (PR #167) pay for itself in accepted-result cost? Keep that change only
   if this rerun supports it.
2. Is Liberado's native loop good enough to keep as the default worker, or only as one backend
   among several? That is the evidence gate for
   [`coding-worker-control-plane.md`](coding-worker-control-plane.md). Do not build that plane from
   this sample. Do not rank harnesses from this sample. Do not change `max_turns` from this sample.

## 2. What this item is not

- A Liberado/Pi comparison without Hermes and Deep Agents. That is evidence. It does not close C3.
- Compare 4 on B1 (`n = 1`). Same.
- The Kilo Code F11 A/B, or the P3.1a runs that followed. Different task, different controls.
- A 10-task dogfood that changes several settings together.
- A change to Liberado's executor, critic, completion gate, or default `max_turns`.
- A ranking, leaderboard, or marketing table.

C5 (completion gate on vs off) waits for this baseline. It is a later item on the same task set.

## 3. Frozen experiment

Record every pin in `job.json` / `experiment.json` / `pins.txt` before the first paid run. Changing
a pin is a new experiment.

| Pin | Value |
|---|---|
| Harnesses | Liberado, Pi, Hermes, Deep Agents. Pin each to an exact commit. |
| Repository commit | One SHA, resolved before `job.json` is written. |
| Task | One task file, hashed, captured under the job input directory. |
| Model / provider | The DeepSeek V4 Flash class Liberado actually uses, through the same provider for every harness. |
| Sampling | `sampling=omitted`. Do not pass a temperature that only one client honours. |
| Resource limits | Same wall-clock, compile timeout, disk floor, and Liberado turn cap on every job. Pi/Hermes/Deep Agents keep their native turn budgets; record those values, do not silently copy Liberado's. |
| Tool surface | `tool_surface=native`. Do not narrow catalogues to make the field look fair. |
| System prompt | Native per harness. A normalized prompt is a later ablation, not C3. |
| Completion gate | Default-off. Measuring it is C5. |
| Verifier repair | Off. The score is native first-pass behaviour. `--verifier-repair-attempts` would mix coordinator recovery into the result. |
| Task-aware context | Off, unless that flag is the declared experiment variable. C3 is the control, not that ablation. |
| Run order | Alternate per job so the "first harness" bias cancels. Record `run_order` in the report. It is not part of the experiment id. |

### 3.1 Task rules

The task must:

- Be unimplemented at the pin commit. A task already landed by hand is not a test.
- Stay inside Liberado's granted write scope. A task that needs out-of-scope writes measures
  policy, not capability.
- Have an independent gate: the common `cargo test --workspace --no-fail-fast` verifier, plus an
  `--acceptance-overlay` when the workspace suite cannot see the required behaviour.
- Be substantial enough that "compiles" is not "works". The P3.1a miss (green `cargo check`, seven
  failing tests) is the failure class this gate exists to catch.

Freeze the task text before spend. Do not edit it between repeats.

### 3.2 Repeats

Run at least three repeats per harness where cost permits. If cost forbids three, publish `n` and
do not report p95. A single paired run is not this baseline.

### 3.3 Fairness that is already encoded

The runner already records these so a silent default cannot split two runs:

- `tool_surface=native`
- `pi_turn_cap=unset` (and the equivalent native cap for each new adapter)
- `run_order`
- `sampling=omitted`

Keep them. Do not add a coordinator allowlist or blacklist. Base protections (`.git/**`,
`target/**`) stay native harness policy.

## 4. Adapter gap

Compiled adapters today are Liberado and Pi (`HarnessAdapter` in `crates/harness-eval`).
Hermes and Deep Agents live as gitignored checkouts and are MIT. They do not implement the trait.

C3 may add **thin launch adapters** so those two harnesses run under the same coordinator. That is
measurement plumbing. It is not a coding-pack feature and it is not the control-plane worker trait.

An adapter may only:

- check its own executable (`preflight`)
- launch inside the assigned worktree (`launch` / `run`)
- produce the common result and, where possible, MVL / execution-log artifacts

The coordinator still owns worktrees, pins, the verifier, preservation, and classification. A new
adapter does not get to change comparison policy. A later Cline adapter uses the same boundary;
Cline is not part of C3.

**Model View Log** from the three forks is the preferred failure-class source
([`model-view-log.md`](../spec/reference/model-view-log.md),
[`execution-log.md`](../spec/reference/execution-log.md)). It is not a C3 blocker. Native session
artifacts may be archived and parsed; metrics that cannot be parsed are omitted, not invented.
Liberado already emits both streams.

Do not fork those harnesses into this workspace as path dependencies. Pin their commits. Launch
them as external processes.

## 5. How to run

Do not assemble long-lived run policy in PowerShell. Use `liberado coder compare`.

Prerequisites, neither of which is created for you:

- `liberado` and sibling `liberado-harness-worker`
- `.liberado/harness-worker.json` (provider allowlist, credential alias, turn ceiling, disk floor)

Sequence:

```text
liberado coder compare doctor --task <task> --commit <sha> \
  --model deepseek/deepseek-v4-flash --provider openrouter \
  --credential openrouter-default

liberado coder compare submit --task <task> --commit <sha> \
  --model deepseek/deepseek-v4-flash --provider openrouter \
  --credential openrouter-default \
  --thinking high --max-turns 400 \
  --compile-timeout-secs 3600 --run-timeout-secs 14400 \
  --minimum-free-gib 20

liberado coder compare await <job-id>
liberado coder compare report <job-id> --json
```

`doctor` spends no model tokens. The spool is `.liberado/harness-jobs/<job-id>/`. One paid job at a
time per repository (`runner.lock`). Check disk before dispatch; a coding run has already died at
0.1 GB free.

The common verifier runs after every harness. A harness exit of 0 does not hide a red suite. Hidden
tests live in `--acceptance-overlay` and are installed only during verify. The ship-bar excerpt
must prefer FAILED / error lines or the first failing package, not the last-N lines of a passing
crate (PR #170).

## 6. What to report

Publish a dated evidence record under `docs/validation/`. Authority: evidence. Include the pin
table, `n`, commands, OS, model/provider versions, job ids, and archive refs.

| Field | Role |
|---|---|
| Ship-gate rate | Common verifier accepted (`accepted` = harness exit 0 and verifier exit 0) |
| Merge-ready rate | Human judgement: would this merge after the recorded repair? |
| Cost per accepted result | Tokens and money, including failed attempts, retries, and reviewers |
| p50 / p95 duration | Only if `n` supports them |
| Human repair | Time or diff still needed after a "success" |
| Failure class + trace / MVL ids | Turns a score into the next testable change |

Use the rest to **explain**, never to rank:

- tokens in / out / cached
- turns used
- edits and edit failures
- reads per successful edit (task shape, not quality — Kilo scored 6.5 then 1.0 and still shipped)
- tools withdrawn

`report.json` already carries per-harness exit, verifier, archived HEAD, `accepted`, and when
artifacts parse: wall-clock, turns, tokens. Do not invent missing metrics.

## 7. Acceptance

C3 is closed when all of the following are true:

1. Four harnesses ran on the frozen pin table in §3.
2. Repeats meet §3.2, or the record states why `n` is smaller and omits p95.
3. The evidence record in `docs/validation/` contains every field in §6 that the sample supports.
4. The record states whether PR #167 (offload) should be retained, with the cost numbers attached.
5. No coding-pack default changed as a side effect of the run.

A Liberado/Pi-only table may be committed as supporting evidence. It does not tick this item.

## 8. After this

- **C5** — same task set, `[coder.gate] enabled` off vs on. Do not change the default first.
- One evidence-selected mechanism at a time. The study's remaining levers (mutation-landed check,
  side-effect classification, cache directives, compaction file-op lists, script-over-RPC) stay
  parked until this table points at one.
- The control-plane plan stays **not scheduled**. This table is its evidence gate, not its start.

## 9. Provenance

The experiment design was spread across the backlog, the 2026-08 harness study, and the runner
reference. This file is now the authority for *what C3 is*. The runner reference remains the
authority for *how `liberado coder compare` works*.
