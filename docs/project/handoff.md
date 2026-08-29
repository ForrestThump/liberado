# Handoff — Liberado is the daily-driver life OS (Telegram + TurboVault plugins)

Docs map: [`README.md`](../README.md) · open work: [`../roadmap.md`](../roadmap.md).

## 2026-07-26 update (engineering + dogfood — report delivery, authority, deploy hygiene)

Branch `fix/delivery-decoupling` (11 commits, 1492 tests). Live on the homelab and smoke-verified.
Two of these were found by dogfooding over Telegram, not by tests.

**Report delivery (`Delivery`).** A subagent's `Report` can now be filed straight to the vault by the
orchestrator instead of paraphrased by the face agent — one deterministic tool call, no model reads
the body, and the face agent gets a receipt. Verified live: a 28,225-byte research report reached
`Learning/` from Telegram with a one-line chat reply. Declared sink in `[topology.report_sink]`,
boot-validated. See `crates/orchestrator/ARCHITECTURE.md` and dispatch spec §7.1.

**`Depth` is declared, not inferred.** Budget, loop profile, and delivery permission were all derived
from one "every MCP is read-only" predicate. They are three different questions, and the conflation
meant a deep-research goal that merely *mentioned* the vault got 8 turns instead of 30 and failed.
Delivery now gates on `CONSEQUENCE_GATE`; `salvageable` stays inferred (a safety property). §7.2.

**Subagents had no zone capabilities, ever.** `subagent_gate_capabilities` intersected the ceiling
with a set built only from `ExecuteMcp`, silently dropping every `Read`/`Write(Zone)`. So every
subagent vault write became a permission request — for months — and nobody noticed because *a denied
write and a deliberately-protected zone produce the identical observable*. The daemon even logged the
grant as present while refusing the write. Fixed; reads were never affected.

**Unattended dispatch may no longer be asked to clarify.** A cron got "how should I proceed?" at
01:55 with nobody there. `Clarify` presupposes an interlocutor; `Capability::AskHuman` already said
so, the `dispatcher` grant already omitted it, `coder-agent` already honoured it — the dispatcher
never consulted the capability set it was holding. Now guard 0. `BlockReason::UnusableOutput` split
from `LowConfidence`, and `complete_json` re-asks once on undecodable output.

**Observability, which is the real deliverable.** Every guard on both enforcement points now names
itself (`guard=`/`verdict=`/`needed=`/`held=`); decode failures log what the model actually said;
`liberado config explain <component> <mcp:tool> <path>` answers "would this be allowed, and if not
why" from config alone. Diagnosis, not safety, was the weak half of this system — every failure this
session was invisible until someone went looking.

**Deploy hygiene.** `deploy.sh` now flocks the build dir and stages per-invocation (deploys were
raced twice in one session; the SHA is stamped from an argument, not the compiled tree, so a race can
produce an image that lies about its contents). `liberado deploy smoke` asserts deployment
*facts* — including that the config actually reached the box, which had already caused a "successful"
deploy of an inert feature.

**Prompt caching: already working.** Looked like the biggest unclaimed cost lever; measurement showed
DeepSeek prefix caching at 93–98%. `Usage::cached_prompt_tokens` now records it.
See [`research/orchestration-report-applied.md`](../future-work/research/orchestration-report-applied.md).

*Known open, deliberately:* checkpointing for long runs (zero crashes observed in 5 deep runs — the
report's 20%→72% figure is for dependent chains, not a ReAct gathering loop); a precise
`decision_schema()`; verifying deploys by capability rather than SHA label; whether the cache tracks
a *growing* prefix on a 30-turn run. Concurrent Telegram turns share one sticky session, untested
with two chatty turns. Subagents now inherit pool zone grants — defensible, unproven by use.

## 2026-07-23 update (engineering — read with 07-19)

- **Architecture hardening landed** on branch `architecture-hardening` (commit message: module splits, T1, MCP pooling):
  - **Module splits:** server API route groups, daemon lifecycle modules, config-loader model sections, executor budget module.
  - **MCP pooling (M1) + M1b degraded routing + topology hot-reload:** default-on pool via `tuning.mcp_pooling`; transport failure marks peers **degraded** so `routing_descriptors()` omits them. **Hand-edited `topology.toml`** remains the peer source; `POST /api/mcp/reload` (and `LiveMcpController::apply_config`) re-applies the MCP slice without process restart. No agent-owned MCPs; no admin registry UI.
  - **T1 live conformance L1–L10:** L1–L8/L10 in `crates/server/src/t1_conformance.rs`; L9 in `liberado-daemon` (`l9_cron_event_becomes_joinable_dispatched_session`).
- Homelab **ops status** below is still the 2026-07-19 dogfood baseline unless you redeploy this branch.

## 2026-07-19 update (ops / dogfood)

- **Liberado is LIVE on the homelab** and is the chat surface for life ops:
  - API: the operator-configured `homelab.api_url` from untracked `ops.toml`
  - Telegram: `@liberado_notification_bot` (sticky free-form chat + cron delivery)
  - Provider: OpenRouter → `deepseek/deepseek-v4-pro`
- **TurboVault plugin work paid off.** The live TurboVault peer (homelab `develop` image,
  `http://turbovault:3001`) exposes the plugin boundary to Liberado over MCP. Liberado via
  Telegram can use:
  - **Vector search** — `vector_*` module (`vector_search` / `vector_reindex` / `vector_status` /
    `vector_config`): semantic search over the vault, embedded engine
    (`turbovault-vector` + fastembed + usearch), index under
    `.turbovault/plugins/vector/`.
  - **Tasks** — vault-native task surface the agent drives in briefs and chat (plugin module on
    `feat/plugin-tasks`, plus core task write tools already in topology). Morning briefs already
    pull real incomplete-task lists (hundreds of items) as the primary task source.
- **OpenClaw briefings cut over.** Daily / evening / weekly schedules run on Liberado, deliver to
  Telegram, and fold into the sticky chat session (quiet-delay so they don't barge mid-conversation).
  Sticky id survives container restarts. Briefing reliability fixed (weather geocode, CalDAV
  relative hrefs) — live briefs return `Succeeded`.
- **Strategy unchanged:** autonomous life-OS daemon first (replace OpenClaw/Hermes), then chat,
  then coding. See [`architecture/positioning.md`](../spec/architecture/positioning.md).
- **Doctrine:** [`architecture/failure-modes.md`](../spec/architecture/failure-modes.md) — live-verify
  every change against the real daemon.

**What's next is in [`../roadmap.md`](../roadmap.md).** Short version below.

---

## TL;DR — where it stands

| Surface | Status |
|---|---|
| Homelab daemon | **Live** — `liberado:dev`, compose under `~/homelab/services/liberado/` |
| Telegram chat + cron delivery | **Live** — free-form replies answer the sticky session; briefs fold in |
| TurboVault MCP peer | **Live** — `develop` image; vector module + tasks in agent reach |
| Provider | **Live** — OpenRouter / `deepseek/deepseek-v4-pro` |
| Vault mount | Liberado `:rw`; writes remain capability-gated in Liberado policy |
| OpenClaw briefings | **Retired** onto Liberado (no double-fire) |

### Operate it

```bash
ssh <operator-host-from-ops.toml>
docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
docker logs liberado --tail 40
docker ps --filter name=liberado
curl -fsS "$LIBERADO_API_URL/api/status"
curl -fsS "$LIBERADO_API_URL/api/models"
```

| Thing | Location |
|---|---|
| **Image** | `liberado:dev` (built ON the homelab) |
| **Build context** | `~/liberado-build/` (path-dep `turbovault` = fork `develop`) |
| **Compose** | `~/homelab/services/liberado/docker-compose.yml` |
| **Config** | `~/homelab/services/liberado/config/{topology.toml,policy.toml}` → `/config:ro` |
| **Secrets** | `~/homelab/services/liberado/.env` (mode 600) — surgical: provider keys + `LIBERADO_TELEGRAM_*` only (do **not** env_file the full `envs/_shared/auth.env`) |
| **Data** | `~/homelab/services/liberado/data/` → `/data` (session store + sticky Telegram id) |
| **Vault** | syncthing Main → Liberado `/vault:ro`; TurboVault peer mounts rw |
| **API** | `homelab.api_url` in untracked `ops.toml` |
| **TurboVault peer** | `http://turbovault:3001` (nginx front; speak to this, not `turbovault-backend`) |

### TurboVault modules (sibling repo)

Work lives in the `turbovault/` sibling (not the life-os workspace). High level:

| Module | Branch / status | Liberado payoff |
|---|---|---|
| **Plugin API** (`turbovault-plugin-api`) | Landed (#39) | Boundary Liberado plugins use |
| **`vector`** | On fork `develop` (prototype Phases 1–4 done); live on homelab with `--features vector` | Semantic vault search from Telegram / briefs |
| **`tasks`** | `feat/plugin-tasks` (extraction + self-tuning + recurrence); core task tools also on `develop` | Life-OS todo surface; briefs already depend on tasks |
| **`vault_events`** | Planned — [`../future-work/turbovault-vault-events-plugin-plan.md`](../future-work/turbovault-vault-events-plugin-plan.md) | Optional L1 perception; not blocking Liberado P1 |

Umbrella: [`../future-work/turbovault-modules-integration-roadmap.md`](../future-work/turbovault-modules-integration-roadmap.md).

### Provider / Telegram / timezone (done — keep these constraints)

- Topology root key: `provider = "openrouter"` **before any `[table]`** (serde silently drops root
  keys if they appear after a table header).
- **`timezone = "America/Chicago"`** (IANA) is the single source of truth for local wall-clock.
  Cron *expressions* stay UTC; the daemon stamps `Local time: …` onto cron/webhook goals at fire
  time. Helpers: `liberado_common::UserTimezone` / `topology.user_timezone()` for any other context.
- Telegram env is **`LIBERADO_TELEGRAM_BOT_TOKEN`** + **`LIBERADO_TELEGRAM_CHAT_ID`** only — never
  reuse OpenClaw's `TELEGRAM_*` names.
- Sticky session id: `<data_dir>/telegram-sticky-session` on the `/data` volume.

### Rebuild path (homelab; Windows has no Docker Desktop)

```bash
# Preferred: GitHub Actions publishes ghcr.io/forrestthump/liberado:sha-<commit>.
git fetch origin
git checkout <branch>
./deploy/homelab/setup.sh
# Fallback on-box image build (does not pull GHCR):
just deploy-homelab
# Then: docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
```

**Do not** use `pgrep -f "docker build"` alone — it self-matches the ssh command string.

---

## What's next for Liberado (priority order)

Strategy is still **daemon → chat → coding**. Modules and MCP breadth *support* the daemon daily-driver
bar; they do not replace it. Full table: [`../roadmap.md`](../roadmap.md).

### Priority 1 — daily-drive the autonomous life OS

1. **Dogfood Telegram.** Sticky free-form chat + cron delivery is the current phone surface and is
   good enough for the present use case. **Lean into using it** so real friction drives the next
   fixes — do not invent Telegram session multiplexing (operator call 2026-07-19). Known skip:
   `/model` catalog dump has no typeahead for model ids (Telegram only autocompletes top-level
   commands, not arguments).
2. **C1 remaining — interactive crons.** Delivery + OpenClaw cutover are done. Next is
   AskHuman-capable schedules ("run this every morning, **ask me if unsure**") via session profiles.
3. **M1b — done.** Pooling + degraded-catalog routing + topology MCP hot-reload. Peers stay
   hand-edited `topology.toml`; reload via `POST /api/mcp/reload`.
4. **T1 — Live conformance suites** ([runbook](../impl/live-conformance.md)). Tier 1 L1–L11 and the
   Tier 3 deployed-daemon runner are built. Tier 2 remains optional.
5. **W1 later — mobile WebUI session view.** Homespun browser UI when Telegram's flat chat is no
   longer enough. **Not** deep-linking background sessions into Telegram (E5-b deprioritized).

### TurboVault follow-through (not P1-blocking, high synergy)

- Land **`tasks`** module onto fork `develop` / upstream curation path (today: `feat/plugin-tasks`).
- Upstream-ready splits for **#42** (`plugin_state_dir`), **#43** (change-feed / `list_notes_meta`),
  and `vector` once Nick reviews shapes.
- **`vault_events`** module next in the modules sequence — consolidates perception; Liberado keeps
  L0 local watcher authoritative until L1 proves parity.
- Optional: turn-budget "battery" for briefs that hit the turn wall
  ([`ideas/turn-budget-battery-idea.md`](../future-work/ideas/turn-budget-battery-idea.md)).

### Priority 2 / 3 (after the daily-driver bar)

- **CH1** WebUI chat maturity; **CH2** chat history search Tier 1 (lexical).
- Coding: resume mid-build (E6-c(b)), open vtcode no-write finding, eval curriculum — only when
  P1 is not the bottleneck.

**Move-on bar:** leave P1 when you **daily-drive without wincing**, not when it is polished.

---

## Orientation for a fresh agent

1. [`architecture/failure-modes.md`](../spec/architecture/failure-modes.md) — **live-verify doctrine**
2. [`architecture/overview.md`](../spec/architecture/overview.md)
3. [`architecture/sessions.md`](../spec/architecture/sessions.md)
4. [`architecture/positioning.md`](../spec/architecture/positioning.md)
5. [`../roadmap.md`](../roadmap.md) — open work in priority order
6. [`../future-work/turbovault-modules-integration-roadmap.md`](../future-work/turbovault-modules-integration-roadmap.md)
7. [`architecture/session-surface-contract.md`](../spec/architecture/session-surface-contract.md)

**Repo notes:** origin may lag local work; do not push unless asked. Local `turbovault` sibling is
co-developed — leave its branch alone unless the task is TurboVault work.

**Security:** never `docker compose config` with full auth.env in the output path. Liberado `.env` is
surgical on purpose.
