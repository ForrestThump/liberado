# Tuning reference — every knob, and which ones need a rebuild

**Who this is for**: you, at 2am, when the daemon did something surprising and you want to change it
without reading Rust.

The short answer to *"is this baked into the binary?"* is: **almost nothing is.** Models, budgets,
timeouts, MCPs, schedules, grants, zones and delivery all live in TOML that the daemon reads at boot.
A handful of safety and loop-control constants are compiled in, and they are listed at the bottom
with the reason each one is not a config field.

---

## 1. Where config lives

Three files in one directory. The daemon resolves that directory in tiers (`liberado_config::config_dir`):

1. `$LIBERADO_CONFIG_DIR`
2. the platform config dir (`~/.config/liberado`, `%APPDATA%\liberado`)
3. the directory containing the running binary

| File | Owns |
|---|---|
| `topology.toml` | *What exists* — vault path, timezone, providers, models, per-role overrides, MCPs, cron schedules, webhooks, pools, session profiles, the report sink |
| `policy.toml` | *Who may do what* — zones and their write classes, per-component capability grants |
| `tuning.toml` | *How it behaves* — thresholds, budgets, timeouts, intervals. Every field has a default; the file is optional |

**On the homelab, config is a host mount and `deploy.sh` does not ship it.** The copies in
`deploy/homelab/config/` are a mirror for review. To change live config:

```bash
# edit ~/homelab/services/liberado/config/{topology,policy}.toml on the box, then
cd ~/homelab/services/liberado && docker compose up -d --force-recreate   # no rebuild
```

Then confirm it took, in-container (the box also applies a machine-owned grants overlay that is not
in the repo, so a local check can disagree):

```bash
ssh <box> 'docker exec -e LIBERADO_CONFIG_DIR=/config liberado liberado config check'
```

---

## 2. The knobs you will actually reach for

### Models and per-role tiering — `topology.toml`

```toml
provider = "openrouter"          # which [[providers]] entry supplies inference

[roles.dispatcher]               # roles: main_agent | dispatcher | subagent
model = "deepseek/deepseek-v4-flash"
temperature = 0.0
reasoning = "off"                # off | low | medium | high
```

Unset fields inherit the global `provider` and its default model. This is the single biggest cost
lever — a cheap router with a strong worker.

**`[roles.*]` takes a free-form slug and is not checked against anything.** Whatever string you
write is sent to the provider as-is. That is deliberate — it lets you point at a model the day it
ships — but it means a typo surfaces as a provider error at first use, not at boot.

### The model catalog — `[[models]]`

Separate from `[roles.*]`, and easy to conflate. `[roles.*]` says *which slug to call*; `[[models]]`
says *what we know about a slug*. Declaring a model is what makes the daemon able to price it and to
size its context window.

```toml
[[models]]
name = "deepseek/deepseek-v4-pro"   # must match the provider slug EXACTLY to be found
tool_calling = true
structured_output = true
context_window = 128000
tier = "control_plane"              # control_plane | work_plane
# Optional — USD per 1,000,000 tokens. Read at query time; never written to the journal.
input = 0.14
output = 0.28
cached_input = 0.014
```

The first five fields are required; a `[[models]]` entry missing any of them fails config load.
`cost` is an optional coarse ranking hint and is **not** money — the three rate fields are.

Declaring a model buys you exactly two things:

| Declared | Unlocks |
|---|---|
| `context_window` | percentage-based compaction triggers (below). Without it, every conversation uses the hard 48k fallback |
| `input` / `output` / `cached_input` | `liberado-cost` can price that model. Without them it reports tokens with **unknown** cost — never a silent `$0.00` |

Rates are optional individually. A model with only `input` and `output` prices its uncached and
completion tokens and falls back to the `input` rate for cached tokens; anything genuinely
unrateable is reported as unknown rather than guessed.

**`[model_roles]` is the checked path.** Where `[roles.*]` is free-form, `[model_roles]` assigns a
*declared* model to a role and is validated at load against that role's capability floor (Decision
13): the dispatcher requires `structured_output = true`, main agent and subagent require
`tool_calling = true`. Referencing an undeclared model, or one that misses its floor, refuses to
boot rather than breaking the dispatch protocol at runtime.

```toml
[model_roles]
dispatcher = "deepseek/deepseek-v4-flash"   # must be a [[models]] name, and must be structured
```

**An empty `[[models]]` list is legal and is the default.** Nothing breaks — you get unpriced cost
reports and 48k compaction everywhere. That is the state a fresh deployment is in.

### Context compaction — `topology.toml`

```toml
[main_agent.compaction]
enabled = true                   # default ON; a reliability guard that is opt-in is off in practice
trigger_pct = 0.75               # fraction of the model's context_window
# trigger_tokens = 48000         # absolute; when set, overrides trigger_pct globally
keep_recent_turns = 3            # user turns kept verbatim after the summary
summary_max_tokens = 1024        # cap on the summary, so the cure can't become the disease
tool_result_max_chars = 2000     # per-tool-result truncation in the summarizer's transcript

[main_agent.compaction.models."deepseek/deepseek-v4-pro"]
trigger_tokens = 96000           # this model only; absolute wins over any percentage
```

Triggers are **absolute estimated-token counts**, resolved per model at boot. Estimation is
`chars / 4 × 1.3` — deliberately a little conservative, because provider tokenizers undercount code
and JSON.

Resolution, first match wins:

| # | Source | Condition |
|---|---|---|
| 1 | `[main_agent.compaction.models."<slug>"].trigger_tokens` | per-model absolute |
| 2 | that model's `trigger_pct` × its `[[models]].context_window` | per-model pct, model declared |
| 3 | `[main_agent.compaction].trigger_tokens` | global absolute, when set |
| 4 | global `trigger_pct` × the model's `context_window` | model declared |
| 5 | **48,000** | fallback — no declared window, no absolute |

Each conversation resolves against **its own** model, not a single process-wide number: a chat
pinned to a 128k model and one on a 64k model get different thresholds, and swapping the daemon-wide
face model retunes only conversations that never pinned one.

`keep_recent_turns` is anchored on user messages, which is what guarantees an assistant's
`tool_calls` and its `tool` results can never be split across the summary seam.

### Cron schedules — `topology.toml`

```toml
[[schedules]]
name = "evening-debrief"
enabled = true
cron_expr = "0 55 1 * * * *"     # SECONDS MINUTES HOURS DOM MONTH DOW YEAR — always UTC
goal = """…"""                   # the prompt; the daemon prepends "Local time: …"
```

`cron_expr` has **no timezone field** — it is UTC, and `topology.timezone` only affects the local-time
string stamped onto the goal. Re-check the offset when DST flips.

To test a schedule without waiting: set `cron_expr` a few minutes out, `up -d --force-recreate`,
watch, then restore. Verify the restore.

### Turn budgets — `topology.toml`

```toml
research_max_turns = 30          # ceiling for depth = "deep" subagents
```

The dispatcher chooses `depth` (`shallow`/`normal`/`deep`) per dispatch; this caps the deep end.

### Report delivery — `topology.toml`

```toml
[report_sink]
mcp = "turbovault"
tool = "write_note"
path_arg = "path"
content_arg = "content"
```

Omit the section entirely and vault delivery is unavailable — every report is summarized by the face
agent instead. Boot-validated: a sink naming a missing, disabled, read-only, or non-writing tool
refuses to start.

### WebUI composer — `topology.toml`

```toml
[webui]
enter_key = "send"      # send | newline
```

What the Enter key does in the browser chat composer.

| value | Enter | how you send |
|---|---|---|
| `"send"` (default) | sends the message | Enter; Shift+Enter for a newline |
| `"newline"` | inserts a newline | the Send button, or Ctrl/Cmd+Enter |

Exactly one of the two, never both — the setting exists because Enter was doing both on mobile. In
`"newline"` mode nothing reachable by a single keypress can send, which is usually what you want on a
phone, where Enter is the easiest key to hit and a mis-send cannot be taken back.

Presentation only: it grants no authority and changes nothing about what an agent may do, which is
why it sits at the top level rather than under `[main_agent]`. The WebUI reads it from
`GET /api/status`; a daemon that does not report it is treated as `"send"`.

**Takes a daemon restart, then a page reload — both.** Config is read at boot, so editing this file
alone changes nothing: `/api/status` keeps reporting the old value until the container is recreated
(verified 2026-08-01, where it did exactly that). The browser then needs a reload to pick the new
value up. It is not baked into the wasm bundle, so no rebuild or redeploy of the WebUI is involved.

`enterkeyhint` on the composer follows this setting, so a phone keyboard's action key changes with
it — a return arrow in `"newline"`, a send glyph in `"send"`.

### MCPs — `topology.toml`

```toml
[[mcps]]
name = "liberado-spider-mcp"
consequence = "read_only"        # read_only | reversible | irreversible | external
transport = { kind = "http", url = "http://…" }
writes_vault = false             # or declare zone_from_arg + write_tools
```

`consequence` is the safety rating and drives the gate — see §4. A non-`read_only` MCP must say where
its writes land or explicitly say it makes none; the daemon refuses to boot otherwise.

### Who may do what — `policy.toml`

```toml
[[zones]]
zone = "Learning"
write_class = "agent_writable"   # human_only | agent_writable | proposal_only | shared

[[grants]]
component = "dispatcher"         # main-agent | dispatcher | life | coding
capabilities = [
    { Read  = { Vault = "Learning" } },
    { Write = { Vault = "Learning" } },
    { ExecuteMcp = "turbovault" },
    "AskHuman",                  # may this actor ask a person? crons must NOT have this
]
```

An **undeclared zone defaults to `proposal_only`** — a write there raises an approval request rather
than failing. That is the fail-safe, and the usual cause of "why is it asking me?".

**Debug an authority refusal without guessing:**

```bash
ssh <box> 'docker exec -e LIBERADO_CONFIG_DIR=/config liberado \
    liberado config explain dispatcher turbovault:write_note Learning/x.md'
```

It prints every guard's verdict and the config edit that would fix each failure.

### Zones on a **non-vault** CRUD surface — `topology.toml`

Zones are not vault-specific, and the policy layer never was: `Policy::write_class` keys on the zone
*name*, with no idea what a vault is. What differs per MCP is only **how a call's zone is worked
out**, and there are two declaration styles:

```toml
# Path-addressed — the zone is the leading path segment of an argument. Use when one tool can
# land in different zones depending on the call. This is TurboVault.
[[mcps]]
name = "turbovault"
zone_from_arg = "path"           # write_note(path="tasks/x.md") -> zone `tasks`
write_tools = ["write_note", "delete_note"]

# Fixed-zone — the zone follows from the tool name alone. No paths involved. Use this for any
# other CRUD surface: a billing API, a database, a device.
[[mcps]]
name = "stripe"
default_zone = "finance"         # every tool here writes to `finance` unless overridden

[[mcps.tools]]
name = "get_balance"             # `zone` omitted = explicitly NOT a zone write (the one read
                                 # tool in an otherwise all-write MCP)
```

A non-`read_only` MCP must declare **one of three** things — `default_zone`, `zone_from_arg` +
`write_tools`, or `writes_vault = false` — and the daemon refuses to boot otherwise. That is F1: zone
declaration used to be opt-in, nobody opted in, and both the capability guard and the write-class
guard sat permanently inert. A guard that is off by default is not a guard.

Both styles feed the same guards — capability check, write class, proposal downgrade — so a new
surface inherits the whole model by declaring one of them. Zones you never list in `policy.toml`
default to `proposal_only`, so a surface added before its policy is written asks before it acts.

**A write whose zone cannot be determined is refused, not ignored.** A path-addressed tool called
with no path is `Undeterminable` and fails closed; that is deliberate, and it is the property that
keeps a new surface from quietly escaping the model.

**Name your zones distinctly.** `Zone` carries a `Vault`/`Named` variant, but a zone's identity is
its **name** — `Zone::vault("finance")` and `Zone::named("finance")` are the same zone, because
`policy.toml` has only ever keyed on the name. If a vault folder `finance/` and an external billing
system must be different authorities, give them different names.

### Behaviour thresholds — `tuning.toml`

All optional, all defaulted. The ones worth knowing:

| Section | Field | What it does |
|---|---|---|
| `dispatch` | `clarify_threshold_write` | confidence floor below which an action becomes a Clarify |
| | `max_concurrent_subagents` | parallel dispatch fan-out cap |
| | `narrow_direct_tools` | whether `relevant_mcps` narrows the executor's tool catalog |
| | `guidance_match_floor` | how confident procedural memory must be to short-circuit routing |
| | `detach_soft_timeout_secs` | when an awaited dispatch is promoted to background |
| `concurrency` | `max_reaction_depth` | stops runaway reaction cascades |
| | `window_secs`, `retry_max` | loop-breaking window and retry cap |
| `context` | `max_goals`, `max_decisions`, `decision_recency_days` | how much context the agent is given |
| `mcp_pooling` | `enabled`, `idle_ttl_secs`, `max_in_flight_per_name`, `connect_wait_secs` | MCP connection pool |
| `cron_delivery` | `quiet_delay_secs`, `deliver_by_secs` | holds a brief until you are between messages |
| `telegram_approvals` | `getupdate_timeout_secs`, `poll_retry_backoff_secs` | long-poll behaviour |
| `proposals` | `reap_interval_secs` | how often expired proposals are swept |
| `capture` | `inbox_path`, `capture_paths`, `ready_flag`, `hold_flag`, settle windows, ignore globs | inbox capture + watcher scope (F12) |
| `maintenance` | git commit + maintenance schedules, `prune_requires_proposal` | vault housekeeping |

---

### Unattended coding runs — environment

Three knobs on the headless/agent path live in the environment rather than a TOML file, because
their natural scope is "this whole runner invocation" rather than "this deployment".

| Variable | Default | What it does |
|---|---|---|
| `LIBERADO_CODER_PUSH` | unset (off) | `1`/`true` makes `liberado-coder-run` **push** the branch it commits after a run. It always commits locally — that is what makes a run's output survive the workspace being deleted — but publishing to a shared remote is outward-facing, so it stays a deliberate choice. |
| `LIBERADO_CODER_VERIFY_CMD` | unset | Replaces the default `cargo check --workspace --all-targets` acceptance verifier for a non-Rust stack. The non-empty-diff verifier always runs regardless. |
| `SHEPHERD_PROFILE` | `coding-unattended` | Which session profile `pr-shepherd.py` starts goals under. It must name a profile whose grant **omits `AskHuman`**, or every goal parks on an intake question with nobody to answer it. |

`pr-shepherd.py` also reads `SHEPHERD_MAX_KICKBACKS` (2), `SHEPHERD_COLD_REVIEWS` (2),
`SHEPHERD_MAX_CONCURRENT` (2), `SHEPHERD_POLL_SECONDS` (120), `SHEPHERD_BASE` (`main`),
`SHEPHERD_PROJECT` (`liberado`), and `LIBERADO_SERVER` (`http://localhost:4201`).

## 3. Reading what the daemon decided

Config tells it what to do; these tell you what it did.

```bash
# which guard blocked something, and what would have allowed it
docker logs liberado 2>&1 | grep -aE "guard=|authority decision"

# routing: depth, budget, delivery, salvage
docker logs liberado 2>&1 | grep -a "dispatching subagent"

# delivery outcome, and the reason if it downgraded
docker logs liberado 2>&1 | grep -aE "delivered report to vault|delivery downgraded"

# classifier failures — includes finish_reason, token count, and the failing bytes
docker logs liberado 2>&1 | grep -a "did not decode"
```

Latency and token usage (including cache hit rate) are journaled to
`<data-dir>/latency/*.jsonl`; `deploy/homelab/latency-report.sh` summarises per-role p50/p95.

**Which compaction thresholds actually loaded**, at boot:

```bash
docker logs liberado 2>&1 | grep -a "automatic context compaction"
# chat: automatic context compaction enabled (per-conversation model triggers)
#   face_model=… trigger_tokens=48000 models_with_triggers=1 trigger_pct=0.75 keep_recent_turns=3
```

`models_with_triggers` counts declared `[[models]]` plus the live face slug. **A value of `1` means
you have no `[[models]]` entries at all** — the face slug alone was registered, and every
conversation is on the 48k fallback. Also worth grepping: `has no matching [[models]] entry`, warned
when models *are* declared but the face slug does not match one exactly.

**Token cost**, priced at read time from the journal:

```bash
liberado-cost --data-dir <data-dir> --topology <path/to/topology.toml>
```

`--prices` is an alias for `--topology` and lets you keep rates in a standalone file: every
top-level topology key defaults, so a file containing nothing but `[[models]]` entries is accepted.
The *entries themselves* are not lenient — a model missing `tier` (or any of the five required
fields) is a hard parse error, in a rates-only file exactly as in the real topology.

Two read-only analysis tools sit beside it, both reading records the daemon already writes — no
inference, no extra instrumentation:

```bash
# What does delegating cost the turns that follow it?
liberado-cost delegation-cost  [--data-dir PATH] [--json]
# Which delegated answers did the model mostly write itself? (provenance, not quality)
liberado-cost provenance-ratio [--data-dir PATH] [--threshold RATIO] [--json]
```

Both were `cargo run --example` until they earned a place on the CLI (D1). `--data-dir`, `--json`,
`--topology` and `--prices` are global, so they read the same before a subcommand as after one, and
the bare `liberado-cost …` form still runs the cost report.

`provenance-ratio` compares what the face agent *received* from a delegation against what it then
*wrote*. It flags rather than judges — a short lookup expanded into a readable sentence is fine — but
it independently ranked the known [seam bug](../../future-work/archive/delegated-work-is-discarded-at-the-seam.md)
first at 29x against a median of 0.9x. Why this rather than an eval harness:
[`evals_implementation.md`](../../future-work/research/evals_implementation.md).

Money is never written to the journal — only tokens are. Rates come from `[[models]]` at query time,
so repricing history is a config edit, not a migration. A model with no usable rate is listed under
*"models with no usable rate (tokens known, cost unknown — never 0.0)"* and counted in each row's
`unpriced` column, rather than folded into the total as free. Calls made outside any correlation
scope land in the
`(unattributed)` bucket; the journal is append-only, so fixing an attribution gap stops the bucket
growing but does not retroactively empty it.

---

## 4. What is compiled in, and why

These are **not** config fields. Changing one is a code edit plus a rebuild. Each is deliberate.

| Constant | Value | Why not config |
|---|---|---|
| `CONSEQUENCE_GATE` | `Irreversible` | The safety threshold itself. Two independent guards read it; making it operator-editable makes "how dangerous is too dangerous" a runtime accident |
| `DEFAULT_MAX_TURNS` | 8 | Baseline subagent budget. `depth` + `research_max_turns` are the intended knobs |
| `DIRECT_MAX_TURNS` | 4 | `ExecuteDirect` is the "a few steps clearly suffice" path; a large value contradicts the routing decision |
| `WRAP_UP_TURNS` | 3 | Reserve for filing partial work at budget exhaustion |
| `DOOM_LOOP_THRESHOLD` | 3 | Repeats before the loop guard fires |
| `ARG_SIMILARITY_THRESHOLD` | 0.2 | Near-duplicate argument detection (semantic profile only) |
| `DOOM_LOOP_RECOVERY_BONUS_TURNS` | 2 | Extra turns granted after a loop-break nudge |
| `INSTRUCTION_SCAN_LIMIT` | 600 | How much of a goal the magnitude heuristic reads. A tunable value here is a tunable safety guard |
| `MIN_DELIVERED_DOCUMENT_BYTES` | 400 | Floor for "this is a document, not a status line" |
| `DECODE_RETRIES` | 1 | Retries on undecodable classifier output |

**If you find yourself wanting to change one of these from config, that is a signal worth taking
seriously** — either the default is wrong for everyone (change the constant), or the knob genuinely
belongs to the deployment (promote it, as `research_max_turns` was). Do not add a config field for a
safety threshold without deciding, explicitly, that operators may lower it.

---

## 5. Fast answers to likely questions

**"Why is it asking me for approval?"** → `config explain` on the write. Usual causes: undeclared
zone (defaults `proposal_only`), missing `Write` grant, or an MCP rated `irreversible`/`external`.

**"Why did the cron fail with 'blocked'?"** → An unattended actor holds no `AskHuman`, so a Clarify
becomes `Unattended` rather than a question nobody can answer. The message names what to fix, and the
classifier's own question is preserved ahead of it. Grep `guard=ask_human_capability`.

**"Why did my research report go to chat instead of the vault?"** → `grep "delivery downgraded"`; the
`reason=` field says which of the six checks failed.

**"Why is it only getting 8 turns?"** → The dispatcher chose `depth = normal`. Depth is per-dispatch
and declared, not inferred from which MCPs are in scope.

**"I changed config and nothing happened."** → `deploy.sh` ships code, not config. Recreate the
container, then confirm with `config check` **in the container**.

**"Why does the cost report say everything is unpriced?"** → No `[[models]]` entry carries
`input`/`output` rates for those slugs. The report is telling the truth: it will not invent a rate,
and an unpriced model is never counted as free. Declare the slugs with their real rates — config
only, no rebuild.

**"Why is compaction always firing at 48k no matter what I set `trigger_pct` to?"** → `trigger_pct`
is a fraction of `context_window`, and `context_window` only exists on a `[[models]]` entry. With no
declared model there is nothing to take a percentage *of*, so resolution falls to the hard 48k
(row 5 above). Either declare the model with its window, or set an absolute `trigger_tokens`.

**"I set `[roles.main_agent].model` — why isn't it priced / windowed?"** → `[roles.*]` and
`[[models]]` are different tables. The role override picks the slug to call; the catalog entry is
what the daemon knows about it. Pointing a role at a slug does not declare it. The two must name the
slug **identically** — matching is exact, not fuzzy.
