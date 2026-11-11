---
kind: finding
status: historical
authority: evidence
domain: product
canonical_for: opus-review-2026-10
---

# Liberado: an outside review (October 2026)

> Point-in-time external review of `main` at `f027c155` (October 2026). It is not current truth. Where a claim here disagrees with the backlog, [the backlog](../future-work/backlog.md) wins, including step 3 (this text says "cut the gate tax"; the backlog does not).

**Reviewer:** Claude (Anthropic), not part of the Grok model family that wrote the code.
**Scope:** `main` at `f027c155` (2026-10-20), plus open PR #317. I read the code, docs, CI, and
history. I built nothing and changed nothing. Line numbers refer to that commit.
**Status:** a review artifact. It is not committed.

---

## 1. Executive summary

- **The code mass does not match the direction.** There are 322,000 lines of Rust in 58 crates,
  written in four months. About 90,000 lines are the coding pack. About 1,200 lines are the
  reminder engine (`crates/daemon/src/vault_pings/`). Tasks and reminders are the first
  priority in `docs/direction.md`, yet only about 3% of commits touch them.
- **The first-priority feature is not reliable.** A timed reminder fires only if the tick runs
  in the exact same local minute. The reminder is marked "sent" before delivery. Delivery runs
  as a detached task and only logs a warning if it fails. Cron has no catch-up after a restart.
  A deploy at 08:59 loses the 09:00 reminder, and so does a Telegram outage. No alert tells you.
- **The control plane is open.** The HTTP API binds `0.0.0.0:4201`, has no authentication, and
  uses `CorsLayer::permissive()`. Any device on the network, any web page in your browser, or a
  coding agent's shell can start agents and resolve approvals. "Tailscale-only" is an assumption
  about the host network. The code does not enforce it.
- **Capability enforcement at the tool boundary is better than in the references.** The model
  can only narrow its authority. Unknown write tools fail closed. Proposals carry an HMAC. Keep
  this, and close the HTTP hole around it.
- **Liberado is not comparable to Grok Bot, OpenClaw, or Hermes on breadth, and it should not
  try to be.** It has no browser or computer use, no skills system for the personal agent, no
  automatic memory recall, three channels against 25–40, and no installer, onboarding wizard,
  or `doctor` command. Most of these gaps are correct under your direction. Onboarding,
  recovery, and reliability are not, and they need work.
- **"Grok Bot first" conflicts with "Tailscale-only".** Grok Bot runs in the cloud. Its custom
  MCP connectors must be public HTTPS, and localhost needs a tunnel. You must choose between a
  hardened public endpoint, a local client (OpenMausBot, Claude Desktop, Codex) on the tailnet,
  or no Grok Bot access.
- **The docs give agents the wrong work.** `docs/direction.md` says "substrate first". But
  `docs/roadmap.md` (snapshot 2026-09-22), `docs/future-work/backlog.md`, `README.md`, and
  `docs/spec/architecture/positioning.md` still say daemon, then chat, then coding. Agents take
  work from the backlog, and its fallback rows are coding-pack proofs.
- **The quality gates cost more than they return.** The CRAP baseline has 5,777 entries and is
  rewritten in every PR: 15,400 of PR #317's 22,000 changed lines are that file. About 11–13% of
  all commits are metric upkeep. Some commits rename functions only to dodge the CRAP matcher.
  Keep the layer rules and the dependency gates. Cut most of the rest.
- **The data that you call the "single source of truth" has no backup, restore, or export.**
  Only the vault has a `git-snapshot` job. Sessions, approvals, bindings, and reminder state in
  the data directory have none of the three. The health check always returns `running: true`.
- **Next:** lock the control plane, then make delivery durable (outbox, catch-up, alerts), then
  build backup and restore, a task API, and scoped outside access. Freeze the coding pack, and
  reduce the gate tax so each of those steps ships faster.

---

## 2. Capability gap matrix

Sources: the repo for Liberado; official repos and docs for the others (links in the appendix).
Legend: **Yes** = shipped; **Part** = partial or opt-in; **No** = absent.

| Capability | Liberado | Grok Bot (commercial) | OpenClaw | Hermes Agent | Rakazo¹ | OpenMausBot |
|---|---|---|---|---|---|---|
| Install / onboarding | **No**. Clone, `just build`, copy TOML, set env vars. No wizard, no `doctor` | Desktop app, sign in | **Yes**. One-line installer, `onboard` wizard, `doctor --fix`, `security audit` | **Yes**. One-line installer, `setup` wizard, `doctor`, import from OpenClaw | **Yes**. Installer script with Compose, or a Railway template | **Yes**. Signed macOS app, CLI setup |
| Runtime | Rust daemon, Docker image (GHCR) | Cloud | Node gateway daemon on loopback | Python gateway, serverless backends | TypeScript, Postgres, Docker | Electron, local harness |
| Chat channels | WebUI, Telegram, Matrix (plus TUI, CLI) | Desktop, mobile, Slack, email, X | 40+ (WhatsApp, Signal, iMessage, Slack, Discord, Matrix, …) | 25+ (same class) | Web, desktop, mobile, Telegram, WhatsApp | In-app rooms; phone over Tailscale |
| Long-term memory | **Part**. Vault Markdown + HNSW via `memory-mcp`; tool-driven only; no automatic recall | Yes, per bot (chat-managed) | Yes. `USER.md`/`MEMORY.md`/daily notes, hybrid search, "dreaming" consolidation | Yes. Small capped files, session search, pluggable user models | Yes, per bot | Yes. Markdown files with a load cap |
| Skills | **No** for the personal agent (`skills/` is coder playbooks) | Yes, plus "teach a task" | Yes. `SKILL.md`, ClawHub, learned skills | Yes. Self-written and curated, Skills Hub | Yes (Markdown) | Yes (Markdown import) |
| Schedules | **Yes**. Cron plus structured IANA schedules; English text; code-computed next fire times | Yes, plain language | Yes. Automations, one-shot reminders | Yes. Natural language or cron; no-LLM script jobs | Yes (routines) | Yes |
| Missed-fire catch-up | **No** (`crates/cron/src/lib.rs:13`) | Cloud runs while laptop is off | Not verified | Not verified | Not verified | Webhooks only while app runs |
| Event triggers | Vault watch, webhooks (shared secret) | Slack, GitHub, Linear, email, webhooks | Webhooks, Gmail Pub/Sub, hooks | Webhooks, hooks | Connector events | Webhooks, Composio |
| MCP client | **Part**. stdio and streamable HTTP; no SSE connector, though ADR-0003 promises one | Yes. Catalog, plus custom servers on public HTTPS only | Yes. OAuth, probe, doctor | Yes. OAuth, trust levels | Yes | Yes |
| Outside agents use *your* data | **Part**. `operator-dogfood-mcp` wraps the unauthenticated API, with no scopes | n/a | Yes. `openclaw mcp serve` | Yes. OpenAI-compatible API server | n/a | Yes. MCP server that cannot approve |
| Browser / computer use | **No** | Yes. Cloud computer | Yes. Playwright Chromium, nodes | Yes. Five browser backends, `cua-driver` | Yes. Docker, E2B, … | Yes. Cloud, VM, or local |
| Subagents / teams | **Part**. `delegate`, synchronous; coder fan-out only | Yes. Teammates, group chats | Yes. `sessions_spawn`, fork | Yes. Parallel, per-model | Yes | Yes. "Chief of Staff" |
| Approvals | **Yes**. Code-enforced at invoke; HMAC'd proposals; cards in WebUI, Telegram, Matrix | Yes, plus an "Auto Review" model | Yes. Exec modes; **sandbox off by default** | Yes. `smart` mode is LLM-judged; `--yolo` | Yes. User-chosen | Yes. **New bots start at Full access** |
| Control-plane auth | **No**. Open API, permissive CORS, `0.0.0.0` | Vendor account | Loopback by default, token, device pairing | Dashboard refuses a non-loopback bind without auth | Better Auth, row-level security | Harness on `127.0.0.1` |
| Tasks / reminders as source of truth | **Part**. Obsidian Tasks lines in *your* vault; read-only; at-most-once delivery | No (vendor cloud) | Scheduler holds jobs; no task store | Kanban DB, Apple Reminders skill | No | No |
| Budget / finance | **Part**. External `liberado-budget`; PR #317 open | No | No | No | No | No |
| Backup / restore / export | **Part**. Vault git snapshot only | Per-file download; export is Enterprise-only | `doctor` migrations; files on disk | Files on disk; trajectory export | Postgres | Files under `~/.openmausbot` |
| Observability | `tracing`, JSONL journals, coder traces; thin health; no metrics | Vendor | JSONL logs, OpenTelemetry, diagnostics page | Logs, `insights`, OTel and Langfuse plugins | Not verified | Not verified |
| Model independence | **Yes**. Any OpenAI-compatible endpoint; config fallback | No (xAI) | Yes | Yes (18+ providers) | Yes (via Pi) | Yes (your CLI subscriptions) |
| Security record | No public exposure; small surface | Vendor-managed | 722 advisories, CVE-2026-25253, ClawHavoc skill malware, 17k–42k exposed instances | Audit with 4 critical and 9 high; webhook RCE (fixed) | Beta | Permissive defaults; plaintext keys |
| Community | 1 owner | Commercial | ~393k stars, foundation | ~257k stars | ~5.7k stars, beta | ~5.6k stars |

¹ **Rokazo was not found.** Searches for "Rokazo" on the web and on GitHub return no matching
project: `rokazo.com` is a parked domain. A near match, **Rakazo** (`github.com/elie222/rakazo`,
Apache-2.0), calls itself an "open-source Grok Bot alternative". I assume you mean that project,
but please confirm it.

### Where "comparable to Grok Bot or OpenClaw" conflicts with the direction

| Reference capability | Conflict? | Recommendation |
|---|---|---|
| Browser and computer use | **Yes.** The direction says "rent the intelligence". A browser agent is the costliest surface to build and to secure. | Do not build it. Let Grok Bot or another rented agent do browser work. It writes back to Liberado through scoped access. |
| 25–40 channels | **Yes.** Each channel is an auth surface and a reconnect loop. | Keep WebUI, Telegram, and Matrix. Add Web Push, or `ntfy`, only as an *alert* channel. |
| Skills hub, self-writing skills | **Yes.** ClawHavoc and the Hermes audit show this is the main injection path. | No hub. A small, read-only, local `SKILL.md` folder that you own is enough. |
| Multi-agent teammates | **Yes.** It is a non-goal. | Park it. |
| Grok Bot as the first outside client | **Yes, technically.** Grok Bot connectors need public HTTPS. Tailscale-only blocks it. | See open question 1. Start with a client that runs on the tailnet. |
| One-line install, onboarding, `doctor` | **No.** You are the user, and you will rebuild this box someday. | Build `liberado doctor`, and test restore. |
| Reliability of delivery | **No.** This *is* the direction. | Top of the roadmap. |
| Automatic memory recall | **Mild.** It belongs to the "native loop", which is the fallback. | Small: inject a capped `USER.md`-style note into each turn. Skip vector recall. |

The fair comparison is narrower than the matrix. On usefulness to you, Liberado should beat the
references on four points: **a reminder never gets lost, your data stays in files you own, an
outside agent can use that data without being able to approve its own actions, and you can
rebuild the box in an hour.** Today it does not yet win any of the four.

---

## 3. Top risks and weaknesses

### R1. Unauthenticated control plane (high)

- The server binds `0.0.0.0` with no option: `crates/server/src/lib.rs:101`. The quickstart
  documents it: `docs/impl/getting-started/quickstart.md`, "binds `0.0.0.0:4201`".
- The router has no authentication layer. `CorsLayer::permissive()` is at
  `crates/server/src/lib.rs:494`.
- `POST /api/approvals/{id}/resolve` (`crates/server/src/api/approvals.rs:44`) checks no caller
  identity. The code comment says "POST so a web-fetching MCP (GET only) cannot decide". That
  stops one threat and ignores three others:
  - any browser tab, through permissive CORS;
  - any LAN or tailnet host;
  - the coding shell. `CommandPolicy::default()` denies only the program name `git`
    (`crates/coder-core/src/lib.rs:183-206`). The deny rule matches the program stem
    (`crates/coder-sandbox/src/lib.rs:504-560`), so `curl` and `sh -c` both pass. The sandbox
    default is `HostLocal` (`crates/coder-agent/src/session_pack/build.rs:608-633`).
- `deploy/homelab/docker-compose.yml:14-15` publishes `4201:4201`. Docker port publishing
  bypasses host firewalls such as `ufw`.
- ADR-0010 calls for a localhost or Tailscale bind and component authentication. None of it is
  in the code.
- `operator-dogfood-mcp`, the seed of "outside agent access", wraps this API with "no
  authentication scheme" (`crates/operator-dogfood-mcp/src/main.rs:1-15`).

The result: the HMAC on proposals and the code-enforced grants hold *inside* the agent loop. But
anything that can send one HTTP request can go around them.

### R2. Reminder delivery is at-most-once and silent on failure (high)

- `reminder_message` saves fire keys *before* it returns the text:
  `crates/daemon/src/vault_pings/tick.rs:62-67`.
- `send_reminder` spawns a task and only warns on failure: `crates/daemon/src/jobs.rs:216-226`.
  If no notifier is configured, the reminder is still marked as sent.
- The match is the same minute only (`tick.rs:203-205`). A slow tick, a paused container, or a
  restart over that minute loses the reminder for good.
- "No catch-up/backfill on restart" (`crates/cron/src/lib.rs:13-14`, ADR-0020).
- The fire-key file is written without an atomic rename (`tick.rs:259-274`). The code comment
  says so.
- Every minute, the tick reads every Markdown file in the vault. That cost grows with the vault.
- The fire key uses the task's position among task lines (`tick.rs:126`). An edit above the task
  changes the key and can cause a duplicate.
- PR #317 adds a *second*, separate catch-up and dedupe design for the weekly spending job,
  instead of one shared outbox.

### R3. The direction lives in one doc; the agent work queue lives in another (high)

`docs/future-work/backlog.md` says it "is the only queue from which an agent should select
implementation work". Its rows are CAS4 dogfooding, then R1 Codex PR review, then the C3
cross-harness baseline, the completion gate, token economics, TurboVault, and Jev. No row covers
tasks, reminders, budget, backup, or outside access. `README.md` still says "Daemon (life-ops)
first → chat surface → coding pack". `positioning.md` still says Liberado "stands in for all
three categories". Your agents do what the backlog says, so they will drift.

### R4. Effort goes to the coding pack and to process (medium–high)

- The coding pack is `coder-*`, `acp-bridge`, `harness-eval`, `heuristics-tuner`,
  `provider-free-proxy`, `eval`, and `conformance`: about 96,000 lines, or 30% of the Rust.
  About 24% of commits mention coder, harness, shepherd, review, ACP, or mutants work.
- About 15,000 of the 17,600 lines in the `liberado` CLI are developer process: `ci`, CRAP,
  `module_health`, `unwrap`, `mutants`, `docs_audit`, `readiness`, `shepherd`, and `compare`.
  It has no user command for `backup`, `restore`, `export`, or `doctor`.
- `sysmap-gui`, `sysmap-cli`, `heuristics-tuner`, and `provider-free-proxy` have no workspace
  dependents. CI still compiles them.

### R5. The quality gates shape the code (medium)

- There are four overlapping complexity ratchets: CRAP (5,777 entries), function complexity
  (7,866), module health (882 files), and unwrap classification (111).
- Commit examples: "rename schedule edit helper so CRAP does not match tools::apply" and "stop
  CRAP matching conversation patch to tools::apply". A 3-line serde default lives in its own
  file because "`weekly_spending.rs` is already past the function review boundary" (PR #317,
  `weekly_spending_defaults.rs`).
- Every PR rewrites the baseline JSON. That buries the real diff for any human reviewer.
- The ratchets did not stop the large files: `coder-tools/src/lib.rs` (4,195 lines),
  `orchestrator/src/lib.rs` (3,245), `executor/src/lib.rs` (2,240), and
  `telegram-approvals/src/lib.rs` (1,865).
- The Windows test job (about 15 minutes) is the critical path on every PR, yet the deployment
  target is Linux.

### R6. No recovery story for the data you call the source of truth (medium–high)

- `<data_dir>` holds sessions (JSONL), `approvals.jsonl`, channel bindings, Matrix transaction
  IDs, and the reminder fire file.
- Nothing backs up that directory, exports it, or tests a restore. The conversation and session
  stores have no schema migrator.
- `GET /api/status` always reports `running: true` (`crates/server/src/api/status.rs:29-53`), and
  Compose uses it as the health check. A daemon with a dead vault watcher, a revoked Telegram
  token, and a failed provider still reports healthy.

### R7. Webhooks and scheduled goals (medium)

- Webhook idempotency lives in memory only. ADR-0006's durable journal "was never actually
  built" (`crates/server/src/hooks.rs:83-89`).
- Webhook `context` text is appended to the goal, so the webhook sender can put text into the
  prompt (`hooks.rs:199-205`).
- Scheduled agent goals go through the router classifier by default (`direct` defaults to
  false). The code's own comment says a parse failure "fails closed … and silently does nothing"
  (`crates/cron/src/lib.rs` `Schedule::direct`). An unattended automation should never depend on
  a classifier unless you opt in to it.

### R8. Telegram button taps skip the sender check (medium–low)

Ordinary messages pass `message_is_from_allowed_chat` (`crates/notify/src/lib.rs:539-545`).
`callback_query` updates do not (`:516-536`). This matters only if the approval chat is a group,
or if the bot is ever added to another chat.

### R9. The agent loop costs more than it needs to (medium)

- 56% of measured token spend is "re-sending the run's base context on every hop"
  (`docs/future-work/token-economics-findings-2026-08.md:37,142`). That is roughly 11,000 tokens
  of tool schemas on each orchestrator step.
- Each chat turn goes through a face, `delegate`, the dispatcher, the orchestrator, and the
  executor. That is three model roles for one request.
- There is only an OpenAI-compatible provider. There is no native Anthropic client.
- Memory is recalled only when the model calls a tool. The example config runs `memory-mcp`
  from `target/debug/` (`config.example/topology.toml:327`).

### R10. Single-owner maintainability (medium)

- 206 docs files (about 38,000 lines), 20 ADRs, and 58 crates.
- `AGENTS.md` calls itself "deliberately short" but is 256 lines, and most of it is a record of
  past incidents.
- `config-loader` has about 245 public fields, and the example config has about 143 keys.
- PR #317 hand-writes RFC 8291 Web Push encryption and VAPID signing (about 1,000 lines on
  `aws-lc-rs`) instead of using a maintained crate. It also hard-codes your personal budget
  values (America/Chicago, $240 groceries, a "Tithing" exclusion) as built-in defaults in
  `config-loader`.

This is not a codebase that one person can hold in his head. It is held together by gates that
agents must satisfy, and the gates have become a large part of the product.

---

## 4. What Liberado does better than the references

- **Authority is enforced in code at the tool boundary, and it fails closed.** This is the
  `RiskGatedToolRuntime::invoke` path: capability, vault zone, and consequence
  (`crates/executor/src/risk_gated.rs:324-418`). Unknown path-addressed write tools are refused
  (`crates/common/src/write_target.rs`), and empty grants mean zero authority. OpenClaw ships
  with the sandbox off, Hermes's default approval mode is judged by an LLM, and OpenMausBot
  starts new bots at full access. Liberado's posture at this layer is the best of the six.
- **Approvals cannot be forged from inside the loop.** Proposals carry an HMAC, and the ledger
  is kept outside the vault (`crates/daemon/src/proposals.rs`,
  `crates/common/src/approval_ledger.rs`). The approve action is not a tool.
- **Your data is in your tools.** Tasks are Obsidian Tasks lines and events are Full Calendar
  notes, inside a vault that you already use and back up with git. OpenClaw and Hermes keep
  their source of truth in their own private folders, and Grok Bot keeps it in a vendor cloud.
- **Mechanical jobs need no model.** Digests, habit pings, git snapshots, and reminder ticks run
  with no model call (ADR-0020). They keep working when every provider is down.
- **Schedules are readable and computed by code.** There are structured IANA schedules (#313),
  English descriptions, and `next_fires` previews on the approval cards.
- **Architecture rules are enforced by tests.** `layer_rules.rs`, `config_literal_rules.rs`,
  `workflow_security_rules.rs`, and the `cargo-deny` plus `cargo-vet` admission gate are better
  supply-chain and design discipline than either large reference has.
- **The docs are honest about failure.** `failure-modes.md`, the harness-reliability record with
  its failed hypotheses, and code comments that admit gaps (`hooks.rs:83-89`, `tick.rs:259`)
  make problems easy to find. Many of this review's findings came from Liberado's own comments.
- **The attack surface is small.** There is no skill marketplace, no browser, and no
  multi-tenant web surface. With R1 closed, Liberado would be much harder to compromise than
  OpenClaw.
- **Any model works.** Liberado uses OpenAI-compatible endpoints with config fallback and has no
  vendor-bundled upsell.

---

## 5. Prioritized roadmap (12 steps)

The order is for one owner who ships through AI agents. Each step is one PR, or a short chain of
PRs, that an agent can finish from a clear "done" test. Steps 1–3 are small and make every later
step cheaper or safer.

| # | Step | Size |
|---|---|---|
| 1 | Close the control plane | M |
| 2 | Make the docs agree with the direction | S |
| 3 | Freeze the coding pack and cut the gate tax | M |
| 4 | Durable delivery outbox | M |
| 5 | Schedule catch-up and last-fire state | M |
| 6 | Readiness, `liberado doctor`, and a dead-man alert | S–M |
| 7 | Backup, restore, and export | M |
| 8 | Task and reminder write API with stable IDs | L |
| 9 | Scoped outside-agent access | L |
| 10 | Interactive journal | M |
| 11 | Budget in the substrate (rework PR #317) | M |
| 12 | Cheaper, simpler native loop (fallback only) | M |

### 1. Close the control plane (M)

- **Why:** R1. Every other safety property depends on this one.
- **What:**
  - Default bind to `127.0.0.1`, with `LIBERADO_BIND` to opt in to a tailnet address.
  - A bearer token for the API. Separate per-client tokens for agents, and a human-session
    credential that alone can call `approvals/*/resolve` and `profile`.
  - Same-origin CORS, and `SameSite=Strict` cookies or a CSRF header on POST.
  - Telegram callback sender check.
  - Run coding commands in the Docker sandbox with no route to the daemon port, or deny network
    access by default.
  - Change the Compose port to `127.0.0.1:4201:4201`.
- **Dependencies:** none.
- **Done when:**
  - An unauthenticated `curl` from another tailnet host gets 401.
  - A `fetch` from another origin fails.
  - A test coding run that `curl`s `/api/approvals/x/resolve` is refused.
  - A test in `test-support` fails if someone adds `CorsLayer::permissive` or a `0.0.0.0` bind.

### 2. Make the docs agree with the direction (S)

- **Why:** R3. Agents select work from the backlog.
- **What:**
  - Rewrite `backlog.md` so that this roadmap's steps are its rows.
  - Reduce `roadmap.md` to a pointer to `direction.md` and the backlog.
  - Fix the `README.md` strategy section.
  - Mark `positioning.md` as historical.
- **Dependencies:** none (do it in the same week as step 1).
- **Done when:**
  - The backlog's first open row is a substrate item.
  - A `docs-audit` rule fails if `roadmap.md` names an order that `direction.md` does not.

### 3. Freeze the coding pack and cut the gate tax (M)

- **Why:** R4 and R5. Faster CI and smaller diffs make every later step cheaper.
- **What:**
  - Move `coder-*`, `acp-bridge`, `harness-eval`, `heuristics-tuner`, `eval`, `sysmap*`,
    `provider-free-proxy`, and `conformance` out of the default workspace: either a second
    workspace in the same repo, or a separate repo.
  - Keep `coder-*` buildable, but out of the default `cargo test` and out of per-PR CI.
  - Drop the function-complexity baseline. Keep CRAP only for *new* functions over 30, with no
    per-function ratchet file in git.
  - Run Windows tests and mutants nightly instead of per PR.
- **Dependencies:** step 2.
- **Done when:**
  - Median PR CI time is under 10 minutes.
  - A typical feature PR has no metrics-JSON diff.
  - The default workspace has about 40% fewer lines.

### 4. Durable delivery outbox (M)

- **Why:** R2. "Reliable delivery" is in the direction's own mission line.
- **What:**
  - One SQLite outbox in `<data_dir>` with rows of `(key, channel, payload, due_at, attempts,
    sent_at)`.
  - Every reminder, digest, push, and approval card goes through it. Mark the row as sent only
    after the channel accepts it, and retry with backoff.
  - After N failures, fall back to a second channel (Web Push or `ntfy`).
  - Make writes atomic, and show the outbox on a WebUI page.
- **Dependencies:** step 1 (so that the outbox page is not public).
- **Done when:** fault-injection tests pass for these cases:
  - Telegram returns 500 three times, and the message arrives exactly once on the fourth try.
  - The process is killed between enqueue and send, and the message is sent after restart.
  - The notifier is missing, and the row stays pending and is visible.

### 5. Schedule catch-up and last-fire state (M)

- **Why:** R2. Missed fires are lost today.
- **What:**
  - Persist `last_fired_at` for each schedule.
  - Give each schedule a catch-up policy: `skip`, `once_within(<duration>)` (the default for
    reminders), or `all`.
  - Replace the same-minute match in the reminder tick with "due since the last tick and not in
    the outbox".
  - Index the vault by changed files only, instead of walking it every minute.
  - Make scheduled goals `direct = true` by default.
- **Dependencies:** step 4.
- **Done when:**
  - A test stops the daemon from 08:58 to 09:05, and the 09:00 reminder arrives once at 09:05.
  - DST transition tests for America/Chicago pass, for both spring and autumn.

### 6. Readiness, `liberado doctor`, and a dead-man alert (S–M)

- **Why:** R6. Today you learn about a failure from silence.
- **What:**
  - `/api/health/ready` reports each dependency: vault watcher, provider ping, Telegram
    `getMe`, MCP pool, outbox backlog, and free disk.
  - `liberado doctor` prints the same report, plus a config check and the config directory it
    resolved.
  - A daily "I am alive" message goes to the alert channel. If the message does not arrive, a
    service outside the box (such as healthchecks.io or an `ntfy` scheduled check) tells you.
- **Dependencies:** step 4.
- **Done when:**
  - Revoking the Telegram token turns `doctor` red and makes `ready` return 503.
  - An alert arrives on the other channel within one tick.

### 7. Backup, restore, and export (M)

- **Why:** R6. A source of truth without a restore drill is only a hope.
- **What:**
  - `liberado backup` writes a consistent snapshot of `<data_dir>` plus the vault git
    reference. The format must work with `restic` and `borg`.
  - `liberado restore` reverses it.
  - `liberado export` writes plain files: conversations as Markdown, approvals as CSV.
  - A CI test restores a backup into a temporary directory and boots the daemon.
- **Dependencies:** step 6 (so that `doctor` can verify the restored instance).
- **Done when:**
  - The CI restore test passes.
  - You have run one real restore onto a clean machine, and you wrote down the time it took.

### 8. Task and reminder write API with stable IDs (L)

- **Why:** this is the first substrate priority, and it is read-only today.
- **What:**
  - A `tasks` module over Obsidian Tasks lines, with stable IDs from block references
    (`^lib-xxxx`).
  - Create, complete, snooze, and reschedule, all through the existing vault write gate.
  - Telegram commands `/remind`, `/done`, and `/snooze`, plus snooze buttons on reminders.
  - A WebUI task list.
- **Dependencies:** steps 4 and 5. E4 (TurboVault directory listing) only if the inbox depends on
  it.
- **Done when:**
  - You can create a timed reminder from your phone in under 10 seconds, and it appears in
    Obsidian.
  - Completing it in Obsidian stops any later reminders.
  - An ID survives edits to the lines above the task.

### 9. Scoped outside-agent access (L)

- **Why:** the direction's "Access" layer: other agents act as clients of your data.
- **What:**
  - A streamable-HTTP MCP server with a per-client token, scopes (`tasks:read`,
    `tasks:propose`, `journal:append`, `budget:read`), and rate limits.
  - Every write becomes an approval card. No client can approve.
  - An audit log of every call, and a revocation list.
  - The server stays on Tailscale by default.
  - Write a one-page threat model *before* any public endpoint, and decide the Grok Bot
    question there (open question 1).
- **Dependencies:** steps 1 and 8.
- **Done when:**
  - A client on the tailnet (Claude Desktop, Codex, or OpenMausBot) lists your tasks and
    proposes one, and you approve it on your phone.
  - A revoked token gets 401.
  - The audit log shows all three calls.

### 10. Interactive journal (M)

- **Why:** the third item in your decisions.
- **What:**
  - Telegram capture appends to the Obsidian daily note.
  - A "talk about today" chat links both ways: the conversation ID goes in the note's
    frontmatter, and the note link goes in the conversation.
  - At the end of the chat, the agent writes a summary note.
- **Dependencies:** step 8 (write path). Step 12 helps but is not required.
- **Done when:** you have used it for one week, and each journal conversation can be reached in
  one click from Obsidian and from the WebUI.

### 11. Budget in the substrate: rework PR #317 (M)

- **Why:** budget is the second substrate priority. PR #317 is useful but too big, and it
  hard-codes your personal values.
- **What:**
  - Replace the hand-written Web Push with a maintained crate, or with `ntfy`.
  - Move the bucket values to `topology.toml` with no personal defaults in code.
  - Send through the step 4 outbox, and use the step 5 catch-up instead of the job's own
    catch-up logic.
  - Read-only budget tools for outside agents (step 9).
- **Dependencies:** steps 4 and 5. Step 9 for the outside-agent tools.
- **Done when:**
  - The PR is under 2,000 changed lines, with no metrics JSON.
  - The weekly message survives a Monday restart.

### 12. Cheaper, simpler native loop (M, fallback only)

- **Why:** R9. The native loop is the fallback you keep, so it should be cheap and simple.
- **What:**
  - For chat, remove one model role. The face calls a small per-profile tool set directly, and
    `delegate` is only for long goals.
  - Load MCP tool schemas lazily, by profile.
  - Inject a capped `USER.md`-style memory note into each turn.
- **Dependencies:** step 3.
- **Done when:** a fixed set of 20 chat tasks costs at least 40% fewer tokens, with the same
  pass rate on `liberado-eval`.

---

## 6. Things to stop doing or delete

- **Stop** committing per-function metric baselines in feature PRs. **Delete**
  `function-complexity-baseline.json` and its gate. **Stop** renaming or splitting code to
  avoid the CRAP matcher.
- **Stop** adding to the coding pack: the R1 PR-review proof, the C3 cross-harness baseline,
  shepherd, `heuristics-tuner`, and `provider-free-proxy`. Freeze them out of the default
  build. **Delete** `sysmap-gui` and `sysmap-cli` unless you open them every month.
- **Delete** the old order from `README.md`, `roadmap.md`, and `positioning.md` ("daemon → chat →
  coding", "stands in for all three categories").
- **Stop** WebUI polish PRs at the scale of #309 (18,900 added lines in 68 commits) until steps
  1–8 are done. Your own direction already says this.
- **Stop** writing a new ADR for a mechanism you do not build. ADR-0006 (durable idempotency),
  ADR-0010 (bind and auth), and ADR-0003 (an SSE connector) describe things the code lacks.
  Either build them, or change their status to "not implemented".
- **Stop** hand-writing protocols that have maintained crates (Web Push and VAPID in #317).
- **Stop** putting personal data in code defaults (the Chicago time zone, budget amounts, and
  category names).
- **Shorten** `AGENTS.md` to about 80 lines of rules. Move the incident stories into
  `docs/validation/incidents.md` and link it.
- **Stop** using "a week of dogfooding" as a gate (CAS4, Jev) without a written exit test. It
  has blocked the queue since September.
- **Park** Jev completely until steps 1–9 are done. A prototype that is "off by default" still
  costs CI time, review time, and gate time.

---

## 7. Open questions for Forrest

1. **Grok Bot and Tailscale.** Grok Bot connectors need a public HTTPS MCP server. Which option
   do you want?
   - (a) A hardened public endpoint (Tailscale Funnel or a relay) with per-client tokens and
     propose-only scopes.
   - (b) Use a local client on the tailnet instead, and let Grok Bot reach your data only
     through it.
   - (c) No Grok Bot data access.
2. **Coding pack.** Do you still use the coding pack every week? If not, can I freeze it in
   another workspace or repo?
3. **Ownership of tasks.** Do tasks stay as Obsidian Tasks lines for good, or does Liberado get
   its own task table that it mirrors to the vault? Stable IDs and write safety are much easier
   with the second option. The first option is truer to "integrate before reimplementing".
4. **Missed reminders.** What do you want when a reminder is missed by 2 minutes, 2 hours, and 2
   days? (This sets the default catch-up policy.)
5. **Alert channel.** Which second channel do you trust when Telegram fails: Web Push, `ntfy`,
   email, or Matrix?
6. **Approvals in Telegram.** Is the approval chat a private one-to-one chat with the bot, and
   will it stay that way? (This decides how urgent R8 is.)
7. **Recovery time.** What is your recovery-time target? If the box died tonight, how long are
   you willing to spend getting reminders back?
8. **Quality gates.** Which gates have caught a real bug in the last month? If you can't name
   one for CRAP or module health, may I remove them?
9. **Matrix and Web Push.** You run Matrix (tuwunel) for a single user, and PR #317 adds Web
   Push as well. Is Matrix worth its upkeep?
10. **Rokazo.** Did you mean **Rakazo** (`elie222/rakazo`)? I found no project named Rokazo.

---

## Appendix: sources

- Grok Bot: https://docs.x.ai/grok-bot/overview, https://docs.x.ai/grok-bot/security,
  https://docs.x.ai/grok/connectors, https://cursor.com/help/grok-bot/routines
- OpenClaw: https://github.com/openclaw/openclaw, https://docs.openclaw.ai/concepts/architecture,
  https://docs.openclaw.ai/concepts/memory, https://openclaw.ai/blog/virustotal-partnership,
  https://depthfirst.com/research/1-click-rce-to-steal-your-moltbot-data-and-keys,
  https://thehackernews.com/2026/02/researchers-find-341-malicious-clawhub.html
- Hermes Agent: https://github.com/NousResearch/hermes-agent,
  https://hermes-agent.nousresearch.com/docs/,
  https://hermes-agent.nousresearch.com/docs/user-guide/security,
  https://github.com/NousResearch/hermes-agent/issues/7826
- Rakazo (unconfirmed match for "Rokazo"): https://github.com/elie222/rakazo,
  https://github.com/elie222/rakazo/blob/main/docs/computer-runtime.md
- OpenMausBot: https://github.com/milind-soni/OpenMausBot,
  https://github.com/milind-soni/OpenMausBot/blob/main/docs/approval-levels.md
- Liberado measurements: `git log` on `main` (1,721 non-merge commits), line counts by crate,
  `code-metrics/*.json` entry counts, CI run timings from `gh run list`, and PR #317's diff
  (`git diff --numstat`).
