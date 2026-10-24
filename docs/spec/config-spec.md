# Liberado Config Spec — One Resolved Model, Many Small Files, Fail-Fast

**Status**: Resolves Tier-3 Decision 14 (single source of truth for config / topology). Actionable.
**Owner**: Shiloh Mangus
**Last Updated**: October 16, 2026
**Related**:
- `liberado-architecture-decisions.md` (Decision 14; Decision 10 secrets; Decision 4 policy)
- Every companion spec contributes tunables (their "Tunables — single source of truth" tables)

---

## 1. Principles

1. **Single source of truth = one resolved, validated *model* — not one file.** Many small files are
   loaded and merged into one typed config object at daemon startup. Each individual *setting* is
   owned by exactly one place (the validator rejects duplicate ownership), so single-source-of-truth
   holds at the granularity that matters, without a monster file.
2. **Defaults live in code; config holds only the deltas.** Every tunable has a `Default` impl set to
   the value specced in its home document. The config file contains only what the user wants to
   *change* — a fresh install runs on an **empty or absent** config and still works.
3. **Fail-fast.** The merged config is validated *before* the daemon serves anything. Conflicts are a
   **load-time error, not a runtime surprise**.
4. **Out of the vault, homelab-local.** Config lives on the machine running the system; you `ssh` in
   to change it. It is never synced into the vault (boot-order chicken-and-egg; and a sync conflict
   or phone edit must never be able to alter the containment boundary).
5. **Agents do not write config.** No MCP, hook, dispatcher, or subagent has a capability to modify
   config files. The one exception is §4.5: the Automation Editor files a chat card, and Approve
   calls the schedule writer once. That call splices `[[schedules]]` only. It does not change
   grants. Cron still uses the boot copy until the process is recreated.
6. **Secrets are not config.** They live in env / systemd credentials and are referenced by name
   (Decision 10); raw secret values never appear in any config file.

## 2. The Three Kinds of Config

Config is three different concerns wearing one name; they have different owners, change cadences,
and risk profiles:

| Kind | Examples | Why it lives where it does |
|---|---|---|
| **Topology / wiring** | enabled MCPs/hooks, ports, socket paths, webhook URLs, model/provider selection | The daemon needs the whole picture to wire and boot. Homelab-local. |
| **Security policy** | zones, write-classes, capability grants, secret *references* | The containment surface (Decision 4). **Must be central + auditable in one place** — never scattered per-module, or a module could under-declare its own limits. |
| **Behavior tunables** | thresholds, settle windows, `MAX_*`, schedules | Benign (a wrong value degrades behavior, never breaches containment). Mostly defaults-in-code with optional overrides. |

## 3. File Layout & Precedence

Split by **concern**, not per-module (avoids file proliferation):

```
/etc/liberado/            (or ~/.config/liberado/ — XDG)
├── topology.toml         # wiring: components + enablement, transports, ports/sockets, models
├── policy.toml           # security surface: zones, write-classes, capability grants, secret refs
└── tuning.toml           # OPTIONAL overrides of code defaults; usually small or absent
```

- **Format**: TOML (Rust-idiomatic, unambiguous typing — avoids YAML's coercion surprises). Not in
  the vault, so consistency with Obsidian's YAML frontmatter is not a concern.
- **Table scope is positional.** A header such as `[main_agent]` owns every key after it until the
  next table header. Keep top-level keys such as `vault_path`, `daemon_socket`, and `provider`
  before the first table. This is wrong:

  ```toml
  [main_agent]
  delegation_mode = true
  provider = "openrouter" # main_agent.provider, not the global provider
  ```

  Liberado rejects unknown keys in `[main_agent]` so this mistake fails during config load instead
  of silently leaving the global provider at its `deepseek` default.
- **Merge precedence (lowest → highest):**
  1. **Code defaults** (the `Default` impls).
  2. **Config files** (`topology` / `policy` / `tuning`).
  3. **Environment variables** (`LIBERADO_*`).
  4. **CLI flags** (highest; for one-off overrides).
- Each setting resolves to the highest-precedence source that provides it; everything else falls back
  to the code default. This is why the files can be tiny.

## 4. Validation (the fail-fast contract)

A single loader merges all sources into the typed model, then runs **cross-cutting validation**
before anything starts. Examples of what it rejects:

- A capability or write-class that references a **zone not defined** in `policy`.
- A hook/MCP named in `policy` (grants) that **does not exist / is not enabled** in `topology`.
- **Port / socket-path collisions** across components.
- An enabled hook with **no trigger** (neither subscription routing nor a webhook/timer).
- A **secret reference** with no corresponding env/systemd credential present.
- **Duplicate ownership** of a setting across files.
- Unknown keys in `[main_agent]`, including top-level keys placed below that table header.
- Out-of-range tunables (e.g. `MAX_CONCURRENT_SUBAGENTS = 0`).
- An enabled `[[session_profiles]]` `provider` that is blank, names no `[[providers]]` row, or
  names a row whose `base_url` or `api_key_env` is empty. See §4.4.

### 4.1 Shepherd review validation

Daemon-native review adds strict deployment checks:

- `[shepherd.review].harness_order` contains unique, nonempty worker IDs. It is required and cannot
  be empty when review is enabled for a `controller = "liberado-shepherd"` project.
- Each such writer project names a `[[shepherd.auth]]` row. When review is enabled, that auth row
  has a nonempty `expected_login` so publication can verify the authenticated forge identity.
- Both `[shepherd.review]` and `[[shepherd.auth]]` deny unknown fields. Misspelled publication or
  identity controls therefore stop config loading.

The Slice-4 order places `open_code` immediately after Codex. An enabled OpenCode review worker
must set `permission_mode = "deny_writes"` and either `pricing_policy = "zero_only"` or
`pricing_policy = "named"` with model `openrouter/deepseek/deepseek-v4-flash`. It is not an
OpenAI-compatible or free-router worker. `cursor-local` and `grok-build` are optional ids in the
same `harness_order` list; enable the matching `review_workers` row to use them first or as
fallbacks.

Surfaced two ways:
- **On daemon startup** — refuses to start, prints actionable errors.
- **`liberado config check`** — validates the merged config without starting the daemon (CI-able;
  run after any `ssh` edit before restarting).

## 4.2 Machine-owned overlays and install secrets

Two files live outside the three section files and are written by the running system, never by hand:

- **Grants overlay** (`<data_dir>/grants.overlay.toml`). When a human taps
  "Approve everywhere", the daemon appends the grant here - never into the
  hand-edited `policy.toml`. At boot the overlay is parsed and its
  grants/zones APPENDED to the base policy (base entries keep priority, so
  the overlay can only add authority for a zone the base never declared),
  and the merged candidate is validated like any policy. It is deliberately
  soft: a missing overlay is the common case; a broken one is logged and
  ignored rather than blocking boot. Deleting the file revokes every such
  grant.
- **Proposal signing key** (`<data_dir>/.proposal-key`, 32 random bytes,
  generated on first use). Runtime downgrade proposals are signed with it
  for integrity. If it cannot be persisted, the run falls back to an
  ephemeral key and warns - proposals created then simply fail verification
  after a restart, which is the safe direction (rejected, not accepted).

## 4.3 Pack-section arrival is a composition contract

A value that parses is not a value the runtime uses. `[coder]` rides through
`liberado-config` as an opaque `toml::Value`. The coding pack assembles it with
`liberado_coder_core::CoderTuning::from_value` and `run_config()`. Literal
construction rules catch a surface that hard-builds `CoderRunConfig`. They
cannot catch a setting that defaults and is never the operator's value.

The contract is `crates/config/src/coder_tuning_arrival_tests.rs`. Each row
writes one safety-critical `[coder]` field into a real config directory, loads
it through `load_config`, assembles the run config, and observes the changed
runtime field. Covered today:

| `tuning.toml` key | Silent-default cost if it does not arrive |
|---|---|
| `[coder.gate].enabled` | completion gate stays off |
| `[coder.coder].max_turns` | turn budget ignores the file |
| `[coder.progress].read_only_turn_limit` | progress limiter uses the code default |
| `[coder.hashline].enabled` | hashline edits stay off |

Extend that table one field at a time. Do not add another config framework.

## 4.4 Session profile provider

An enabled `[[session_profiles]]` entry may name the backend that receives its model:

```toml
[[session_profiles]]
name = "dogfood"
model = "deepseek/deepseek-v4-flash"
provider = "openrouter"   # a [[providers]] name
```

`provider` is optional. Omit it and the slug goes to `topology.provider` (the daemon default).
The id is read from the profile on every turn. It is not copied onto the session grant, so an
edit applies to sessions that already pinned `model`. Restart the daemon after changing
`topology.toml`.

Load fails for the cases in §4. Disabled profiles are drafts and skip the check.

`Config::resolve_session_profile` returns `ResolvedProfile`, re-exported from `liberado-config`.
The WebUI rename ledger does not rewrite this file. A reader that only loads `topology.toml`
still sees the operator's names.

An unset API-key environment variable does not fail load. Validation does not read the process
environment, and a missing key on the default provider already boots the daemon watch-only.
Bootstrap warns. A turn on that profile fails and names the missing variable. The slug stays off
the default backend.

A profile that names a provider, including the daemon default, gets its own client when that key
is set. Only an omitted `provider` shares the face client.

## 4.5 Schedule file writer

`[[schedules]]` is the only schedule store. The daemon copies enabled rows into the cron task
at boot and does not hot-reload them. `liberado_config::create_schedule`, `update_schedule`,
and `delete_schedule` are the write path. They take a `topology.toml` path, or the config
directory that contains it. `schedule_topology_path` resolves that file from `config_dir()`.

On apply, in order:

1. If the file is missing or not writable, return that error and write nothing. A read-only
   mount (`/config:ro` in the homelab compose file) fails here.
2. Copy the file to a sibling `topology.toml.bak-<utc>`. If the copy fails, stop. The original
   stays.
3. Splice a `toml_edit` document. Touch only the `[[schedules]]` array: create a row, patch
   the named keys of the row with that name, or delete it. An update starts from the existing
   row and changes only the keys in the patch. It keeps every other key, unknown keys, and
   comments. Removing `job`, `habit_text`, or `deliver` requires those names in `remove`.
   An empty `goal` on a job row is refused. Leave comments and every other table in place. A
   serde rewrite of the whole file is not allowed — it drops the comment blocks and can move
   keys relative to tables.
4. Load the edited text with `load_config` (the boot loader) and therefore `Config::validate`
   (schedule, pool, and profile checks, plus the rest of the merged config). On error, leave
   the original file in place and keep the backup.
5. Write a temp file in the same directory and rename it over `topology.toml`.
6. Return what changed. Cron and `GET /api/schedules` still show the boot copy until the
   process is recreated (`docker compose up -d --force-recreate`). This function does not
   reload either.

The Automation Editor is the caller. `create_schedule`, `update_schedule`, and `delete_schedule`
in the face file a permission card and return. They do not call this writer. Approve on that
card calls it once. Deny writes nothing. A refused write (read-only config directory, or a
validation error) is returned as text on the card and in the chat. The writer still leaves the
original file in place. This path does not hot-reload cron or `GET /api/schedules`, and it does
not change the compose mount.

## 5. Where the Tunables Come From

Each companion spec already defines its tunables and defaults in a "Tunables (single source of truth
— Decision 14)" table. Those tables are the **authoritative list**; the `crates/common` config model
mirrors them as typed fields with matching `Default`s. Concretely, the model aggregates:
- Capability/zone/write-class policy (Decision 4 / concurrency spec) → `policy.toml`.
- Concurrency: `WINDOW`, `MAX_REACTION_DEPTH`, `RETRY_MAX`.
- Dispatch: `SMALL_FANOUT`, tiered `CLARIFY_THRESHOLD`, `MAX_CONCURRENT_SUBAGENTS`,
  `DETACH_SOFT_TIMEOUT`, `dispatcher_model`, `guidance_match_floor`, `subagent.isolation`.
- Context policy: `MAX_GOALS`, `MAX_DECISIONS`, `DECISION_RECENCY`, `header_template`, `inbox_lookback`.
- Capture/inbox: paths, settle windows, flags, `ambient_sweep_schedule`, ignore globs.
- Maintenance/git: commit schedule, `stignore_machine_dirs`, maintenance schedule, prune/merge policy.
- Topology: component enablement, transports, ports/sockets, model/provider selection.

## 6. v1 Scope

- Typed config model in `crates/common` with `Default`s = specced defaults.
- Layered loader (defaults → files → env → CLI) producing one validated model.
- Cross-cutting validation + `liberado config check`.
- Three concern files; all optional (empty/absent config boots on defaults, modulo the minimum
  topology + policy a real deployment needs — e.g. at least one vault path and the daemon socket).

**Deferred (v2+):**
- **Vault-resident tunables** (edit benign knobs from Obsidian on the phone, validated-on-change).
- **User-approval-gated config changes through the system** (agents proposing config edits for
  human approval). Out of scope for initial work — config changes are manual via `ssh`.

## 7. Open Questions — resolved

Both questions below were open as of June 2026; the implementation (`crates/config`) has since
settled them.

1. ~~Config dir location: `/etc/liberado/` vs `~/.config/liberado/`~~ — **resolved**: `config_dir()`
   uses the platform config dir (`dirs::config_dir()/liberado` — XDG on Linux, `%APPDATA%\liberado`
   on Windows), with `LIBERADO_CONFIG_DIR` as an explicit override and a development-convenience
   fallback that walks up from the running binary looking for a `config/` directory. No separate
   `/etc/liberado/` path.
2. ~~Separate files vs sections of one file~~ — **resolved as separate**: `topology.toml`,
   `policy.toml`, and `tuning.toml` ship as three independent files, matching §3's layout.

## 8. Gitignore policy

Local deployment overrides are never committed; only the starter examples are:

| Path | Git status | Purpose |
|---|---|---|
| `config.example/` | committed | Starter files |
| `config/*.toml` | `.gitignore`d | Local deployment overrides |
| `crates/*/config.example/` | committed | Per-crate examples |
| `crates/*/config/*.toml` | `.gitignore`d | Per-crate deployment overrides |

The shipped example is a contract, not a sketch. `crates/config-loader/tests/shipped_config_grants.rs`
parses `config.example/` itself. A fixture that builds its own grants would stay green while the
files we hand to a new deploy go wrong.

The shipped chat-default example (CAS3, [#278](https://github.com/ForrestThump/liberado/pull/278)) pins three facts on those files:

1. `topology.toml` names an enabled `chat-default` `[[session_profiles]]` entry with no `domain`
   (a chat hat) and `component = "main-agent"` (it borrows the face grant).
2. `chat-default` is not in the default `[chat] agent_profiles` set, so create stamps `Chat`.
3. `policy.toml` does not grant `ExecuteMcp = "chat-search"` to `main-agent`. The line is a
   comment on that grant. Uncomment it, and the `[[mcps]]` `chat-search` block in topology, to
   give Chat-shelf look-back. Off by default so a new deploy does not grow the face catalogue.

Product meaning: [`chat-agent-surface-mode.md`](architecture/chat-agent-surface-mode.md) §6d.

## 9. Mechanical schedules

A `[[schedules]]` entry with `job` set does not start a model session. The daemon runs the
built-in kind. Kinds are `git-snapshot`, `task-ping`, `event-ping`, `habit-ping`,
`inbox-if-present`, `vault-reminder-tick`, and `weekly-spending-push`. An unknown kind
is a load error. `habit-ping` requires `habit_text`. `inbox-if-present` requires `goal`,
and that goal runs only when the capture file has text. `vault-reminder-tick` is allowed
only while `[vault_reminders] enabled = true`. `weekly-spending-push` is allowed only
while `[weekly_spending] enabled = true`.

`git-snapshot` commits the vault when it is dirty and pushes. It also runs at startup unless
`run_on_start = false`.

`task-ping` reads Obsidian Tasks lines from vault markdown. It lists open `- [ ]` lines and
in-progress `- [/]` lines. It skips done (`- [x]`) and cancelled (`- [-]`). It reads Tasks
dates on the line: 📅 due, ⏳ scheduled, 🛫 start, and 🔁 recurrence. A due or
scheduled value with a clock (`YYYY-MM-DD HH:MM` or `YYYY-MM-DDTHH:MM`) is shown
as `at HH:MM`. Seconds are ignored. A start date after today hides the line. The list is ranked by Tasks priority (🔺, then ⏫, then 🔼, then
unmarked, then 🔽) and then by due date. Task parsing and status come from TurboVault's
`parse_tasks`. Global task filtering follows Obsidian Tasks settings loaded from
`.obsidian/plugins/obsidian-tasks-plugin/data.json` (`globalFilter` and `removeGlobalFilter`).
When that file is missing or unreadable, no global filter is applied. A tag filter is a token
match (`#task` does not match `#taskforce`). `task_limit` caps the list (default 10).

`event-ping` reads calendar notes from the same vault. A note counts when its YAML
frontmatter has `date: YYYY-MM-DD` and one of these is true:

- It sets a Full Calendar field: `allDay`, `startTime`, `endTime`, or `endDate`.
- It lives under a `calendar/` folder (any case) and sets `title`.

A recurring Full Calendar note (`type: recurring` or `daysOfWeek`) uses `daysOfWeek`
letters `U M T W R F S` (Sunday through Saturday, Thursday is `R`), plus optional
`startRecur` and `endRecur`. `completed: true`, `yes`, or a date skips the note.
`completed: false` or `null` keeps it. The message lists events that overlap today through
the next 7 days, at most 10. There is no CalDAV read and no second event store.

"Today" for both jobs is `topology.timezone`. The default is `America/Chicago`.
A row uses exactly one timing form. `days` plus `at` (`days = ["tue"]`, `at = "20:00"`) is
evaluated in `timezone`, or in `topology.timezone` when that field is omitted, so the clock
stays put across DST. `cron_expr` is the escape hatch: its fields are UTC unless `timezone`
is set, in which case they are civil time in that zone. Set `deliver = false` on these schedules. The job sends on
`LIBERADO_REMINDER_BOT_TOKEN` and `LIBERADO_REMINDER_CHAT_ID`. It does not append to the
Telegram-bound chat, and it does not start a session. If the reminder variables are unset, the
job still runs and the text is logged.

### Timed tick — `[vault_reminders]`

```toml
[vault_reminders]
enabled = false   # master switch; default false
tasks = true      # used only while enabled
events = true
```

`vault-reminder-tick` walks the same vault folders as `task-ping` and `event-ping`. It
sends only notes that opt in. The rightmost marker wins:

- `#remind` as its own tag (any case). `#reminder` does not count. `#remind/trash` does.
- Dataview `remind::` or `reminder::`, or YAML `remind:` / `reminder:`.
  Truthy values are `true`, `yes`, `y`, `1`, and `on` (any case, optional quotes).
  Any other value opts out.

On a task the marker is on that line. On a calendar note it may be in frontmatter or
the body. A task fires when local time matches the first anchor that is present:
📅 due, then ⏳ scheduled, then 🛫 start. That anchor must include a time
(`YYYY-MM-DD HH:MM` or `YYYY-MM-DDTHH:MM`; seconds are ignored). A date with no time
does not fire in the tick. The morning digest lists those. An event fires when it has
`startTime` (a recurring note uses the same weekday expansion as `event-ping`) and
local time matches that clock on the occurrence date. All-day notes do not fire here.
Task parsing and filtering use the same TurboVault `parse_tasks` + Obsidian Tasks global filter
path as `task-ping`, so a remind-marked line without the required global-filter tag does not fire.

The schedule is one UTC minute, for example `0 * * * * *`, with `deliver = false`.
The daemon records fired minutes in `vault-reminder-fires.json` under the data dir,
outside the vault. There is no second task store and no model call. A habit ping such
as tuesday-trash can retire once that task carries `#remind`.

Creating or editing a task or event is a vault markdown edit. TurboVault already writes
those notes. This path has no separate notification record.

See ADR-0020.

### Weekly spending — `[weekly_spending]`

Off by default. The job sends one Web Push for the Monday–Sunday week that just
ended. It does not call a model. List every day on the schedule so a missed Monday
still sends later that week, once. A week that is still unsent after the next Monday
at the open clock is not backlogged.

```toml
[weekly_spending]
enabled = false
timezone = "America/Chicago"
unmapped = "ignore"          # or "count"
# unmapped_bucket = "Everything else"   # required when unmapped = "count"
exclude = ["Rent", "Tithing", "Utilities", "Loans", "Insurance", "Subscriptions", "Inference", "Payroll", "Transfers", "Credit Card", "Medical"]

[[weekly_spending.buckets]]
name = "Groceries"
weekly_dollars = 240
categories = ["Groceries"]

[[weekly_spending.buckets]]
name = "Everything else"
weekly_dollars = 110
categories = ["Dining Out", "Vending", "Housewares", "Fuel & Gas", "Entertainment", "Home Improvement", "Pet Care", "Travel", "Uncategorized"]
```

A missing table uses those defaults, including the two buckets. `buckets = []` with
`enabled = true` is a load error. Category match is the trimmed name, with no case
fold. A category in two buckets, or in a bucket and `exclude`, is a load error.
`weekly_dollars = 0` and a blank bucket name are load errors.

`unmapped = "ignore"` (the default) leaves any other category out of the totals and
logs its name when the spend is not zero. `unmapped = "count"` adds that spend to
`unmapped_bucket`, which must name a bucket. Shipping is the live example of an
unmapped category. The owner chooses ignore or count. Do not guess.

The schedule uses `days` and `at`, with an empty `cron_expr`, in the same IANA zone
as `[weekly_spending].timezone`. Only one enabled row may name the job. A disabled
row may still name it. The row is checked even while the table is off, so a typo
fails at load. Set `deliver = false`. The open clock defaults to 07:00 when the
enabled row is missing. The message covers the previous Monday through Sunday once
local time is at or after that clock on Monday, and also on Tuesday through Sunday.
Before the clock on Monday, the message is still the week before.

Spend comes from the budget service. Set `LIBERADO_BUDGET_API_URL` and
`LIBERADO_BUDGET_API_KEY` in the homelab env file on node-ai. Do not commit them.
The key is sent as `X-API-Key` and is never logged.

Web Push needs `LIBERADO_VAPID_PUBLIC_KEY`, `LIBERADO_VAPID_PRIVATE_KEY`, and
`LIBERADO_VAPID_SUBJECT` (`mailto:…`) in that same env file. Generate a pair with
`npx web-push generate-vapid-keys` or `liberado config vapid-keys`. If any value is
missing or invalid, pushes stay off and the daemon logs the variable names, not the
values. iOS needs the WebUI on a real HTTPS origin. On node-ai the WebUI listens on
port 4201. Expose it with Tailscale Serve, for example
`tailscale serve --bg --https=443 http://127.0.0.1:4201`. This repo does not change
the homelab compose file.

A later change may add a midweek alert when a bucket is already over, or pacing
more than 25% over. That alert is not per transaction, and it is not this job.

## 9a. Direct cron dispatch (`direct = true`)

A `[[schedules]]` entry may set `direct = true` to skip the router (classifier) model and
execute the schedule's `goal` verbatim under the configured grant. Default `false` (today's
behaviour — every schedule routes through the dispatcher).

Why a cron would opt in: the router's job is to disambiguate human chat into Execute /
Clarify / Skip. For an unattended cron with a fully-specified `goal` the router adds nothing,
and a router parse failure (the `UnusableOutput` branch — `liberado-dispatcher`'s `classify`
fallback) degrades to `Clarify`, which fails closed for an unattended trigger that lacks
`AskHuman`. The session finishes as `Failed` with no work done and no recoverable signal.
`direct = true` removes that whole failure mode for the schedules that can describe themselves.

**Scope of the opt-in is the schedule only.** `HookConfig` does not carry a `direct` field;
hook firings keep routing through the dispatcher until a future config schema adds one.

Behaviour:

- The dispatcher synthesizes `ExecuteDirect { seed_calls: [], relevant_mcps: [], confidence:
  1.0 }`. No router / procedural-memory short-circuit / classifier call happens for this
  schedule's firings — the assertion
  `direct_dispatch_bypasses_the_router_and_synthesizes_execute_direct` in
  `crates/dispatcher/src/direct_tests.rs` proves no provider call landed on the mock. The
  hub-attached production path is the same shape: `a_schedules_direct_flag_reaches_the_goal_spec`
  in `crates/daemon/src/tests/helpers.rs` pins that the event's flag reaches `GoalSpec.direct`,
  and `hub_path_with_direct_true_skips_the_router` in `crates/dispatch-pack/src/lib_tests.rs`
  pins that the pack then forwards it as `DispatchRequest.direct` (any router call would fail
  on the empty mock).
- The post-classification pipeline still runs: `enforce_narrow_direct_tools`,
  `sanitize_decision_mcps`, the guard pipeline (capability / consequence / zone / depth / confidence).
  `direct = true` does **not** weaken any structural guard. The synthesized `ExecuteDirect`
  has empty `seed_calls` and empty `relevant_mcps`, so the magnitude / risk-waiver targets the
  heuristic checks are empty, and a sweeping goal text is the case that triggers the
  magnitude downgrade (see `direct_dispatch_still_runs_the_magnitude_guard`).
- `[[risk_waivers]]` cannot rescue an empty-seed direct `ExecuteDirect`: the waiver list keys
  off concrete tool/mcp calls, not goal text, and there are no calls yet. **Reach for risk
  waivers when the goal will fan out to tool calls** (e.g. wide vocabulary matches the
  configured MCPs). For an empty-seed direct run the levers that actually matter are:
  - **do not write sweeping goal text** — the magnitude heuristic still trips on phrases like
    "delete everything", "wipe", "reset", "all of my notes";
  - **set `max_turns`** on the schedule so the executor has a tight envelope even with no
    seeds;
  - **use a narrow `profile`** so the grant carries only the MCPs the goal actually needs.
- The grant is whatever `profile` (the schedule's hat) resolves to. Pair `direct = true` with
  a narrow `profile` so the executor only sees the tools it needs.

The flag is plumbed through the cron crate onto `Event.payload.data["direct"]` and read
back in `crates/daemon/src/helpers.rs::reaction_goal` onto the `GoalSpec`. The dispatch pack
reads `GoalSpec.direct` (`crates/dispatch-pack/src/lib.rs`) and forwards it as
`DispatchRequest.direct` to the dispatcher. That is the path the cron firing actually
takes in production: `Daemon::react` → `react_via_hub` → `reaction_goal` → `GoalSpec` →
dispatch pack → `Dispatcher::route_or_bypass`. The earlier `daemon::dispatch_request` only
saw the unattached call shape and is **not** the production route.

