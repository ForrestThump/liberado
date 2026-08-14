---
kind: plan
status: active
authority: implementation
domain: coding-harness
canonical_for: mvl-live-comparison-prep
open_items: true
last_verified: 2026-08-12
---

# Prep: live comparison of liberado, pi, and deepagents on backlog 0.6

**Status**: Prep only. Do **not** start this run. No dispatch, no paid completion, no
pi/deepagents job.

**Item:** backlog **0.6** / roadmap **4b** — *Emit the joined logs from the common boundary.*
Write append-and-flush JSONL from `executor` / provider request handling, then adapt coding
outcomes to it. Pass the shared crash-survival and reconstruction fixtures. Do not add a second
coding-only source of truth.

This is an open coding task in this repository (`docs/future-work/backlog.md`). It is not a
synthetic toy.

## Shared pins

| Pin | Value |
|---|---|
| Repository commit | `69933c9a8c8c5d64a35ac3d0a10bf1c0465adc1c` (`main` at the plan landing; branch from this SHA, do not mix later work) |
| Provider | OpenRouter (`config/topology.toml` `provider = "openrouter"`) |
| Model | `deepseek/deepseek-v4-pro` (Liberado coder-role default `deepseek-v4-pro` on OpenRouter) |
| Sampling | temperature `0.1`; do not set `max_tokens` (same as `CoderRoleConfig` default) |
| Turn / time cap | 30 model turns; 45 minutes wall clock |
| Native prompts / tools | Keep each harness's own system prompt and tool schemas. Do not copy Liberado prompts into pi or deepagents. |
| API key | `OPENROUTER_API_KEY` (do not start until an operator sets it) |

Shared user task (paste as the prompt; do not edit per harness):

```text
Implement backlog 0.6 / roadmap 4b in this Liberado checkout.

Write append-and-flush Model View Log and execution-log JSONL from the executor /
provider request boundary (not a post-hoc conversion of the end-of-run CoderEvent
document). Adapt coding outcomes to those streams. Pass the shared crash-survival
and reconstruction fixtures in crates/test-support.

Normative contracts:
- docs/spec/reference/model-view-log.md
- docs/spec/reference/execution-log.md

Do not add a second coding-only source of truth. Do not start a live multi-harness
A/B. One PR. Windows is a first-class target.
```

## Output layout (create before a future run; do not create as part of this prep)

```text
$COMPARE/                       # operator-chosen directory, outside the repo
  pins.txt                      # copy of the table above + the SHA
  task.txt                      # the shared prompt
  liberado/
    stdout.log
    stderr.log
    traces/                     # copy of <workspace>/coder-traces/
    mvl/                        # empty until 0.6 emits
  pi/
    stdout.log
    stderr.log
    session.jsonl               # pi --mode json
    mvl/                        # pi/packages/mvl writer, if enabled
  deepagents/
    stdout.log
    stderr.log
    mvl/                        # expected empty: no MVL emitter in this checkout
```

After a future run, point the same Liberado oracle at any MVL that exists:

```text
cargo run -p liberado-test-support --bin mvl-conformance -- \
  --mvl $COMPARE/<harness>/mvl/run.mvl.jsonl \
  --execution $COMPARE/<harness>/mvl/run.execution.jsonl
```

`pi/packages/mvl` is pi's own writer/conformance package. It does **not** replace this oracle.

## Start commands (do not execute)

Print-only helper: `powershell -File scripts/prep-mvl-live-compare.ps1`.
It prints these commands and exits 0. It does not spawn a harness.

### 1. Liberado (ACP path — the Paseo dogfood path)

Reinstall `liberado-acp` from this commit first so the run is not a stale `~/.cargo/bin` binary.

```powershell
$env:LIBERADO_CONFIG_DIR = "C:\Users\Shiloh\Code\life-os\config"
$env:OPENROUTER_API_KEY = "<operator>"   # do not set here
node scripts/dispatch-acp-run.js `
  --cwd C:\Users\Shiloh\Code\life-os `
  --config-dir C:\Users\Shiloh\Code\life-os\config `
  --mode coding `
  --timeout-min 45 `
  --prompt "<paste task.txt>"
```

Headless sibling (different path; not the comparison primary):

```powershell
liberado-coder-run task run `
  --prompt "<paste task.txt>" `
  --workspace C:\Users\Shiloh\Code\life-os `
  --model deepseek/deepseek-v4-pro `
  --max-turns 30 `
  --config-dir C:\Users\Shiloh\Code\life-os\config
```

Liberado traces land under `<workspace>/coder-traces/<session>.json`. Until 0.6, there is **no**
production `*.mvl.jsonl`. The comparison of *work product* (diff, tests, ship bar) can still
happen; the comparison of joined MVL cannot.

### 2. pi (checkout already at `pi/`, gitignored)

```powershell
cd C:\Users\Shiloh\Code\life-os\pi
$env:OPENROUTER_API_KEY = "<operator>"
pi --provider openrouter --model deepseek/deepseek-v4-pro `
  --mode json -p "<paste task.txt>" `
  > $COMPARE\pi\session.jsonl
```

pi may also write `~/.pi/agent/sessions/`. Copy that session file next to `session.jsonl`.
If the checkout's `packages/mvl` writer is wired, copy its JSONL into `$COMPARE/pi/mvl/`.

### 3. deepagents (checkout already at `deepagents/`, gitignored)

There is no MVL emitter in this checkout. Run the library agent against the same repo and
model. Keep the default deepagents system prompt and tools.

```powershell
cd C:\Users\Shiloh\Code\life-os\deepagents
$env:OPENROUTER_API_KEY = "<operator>"
uv run python $COMPARE\deepagents\run_0_6.py
```

`run_0_6.py` (write at run time, not now) should construct `create_deep_agent` with
`model="openai:deepseek/deepseek-v4-pro"` (or the OpenRouter chat-model equivalent),
temperature `0.1`, and invoke the shared task against `C:\Users\Shiloh\Code\life-os`.
Do not add an MVL shim for this comparison.

## Blockers to record in the future report

1. **Liberado cannot emit production MVL until 0.6 lands.** That is the item under test.
   A live run of Liberado on 0.6 will not produce joined MVL *before* the change; only the
   resulting PR might.
2. **deepagents has no MVL writer here.** Score it on ship-gate / merge-ready work, not on
   oracle verdicts, unless a later 0.7 fork adds an emitter.
3. **pi has `packages/mvl`.** That package can write MVL; it is not Liberado's oracle.
   After a pi run, feed its JSONL to `mvl-conformance` if a file exists.
4. This prep is **not** backlog **0.7** / roadmap **5**. It does not publish a baseline.

## What this prep must not do

- Do not call `dispatch-acp-run.js` without `--handshake-only`.
- Do not invoke `pi -p` or `create_deep_agent`.
- Do not set API keys in the repo.
- Do not start Hermes (out of scope).
