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
| `capture` | `inbox_path`, settle windows, ignore globs | inbox capture behaviour |
| `maintenance` | git commit + maintenance schedules, `prune_requires_proposal` | vault housekeeping |

---

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
