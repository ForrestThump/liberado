# Handoff — Liberado is the daily-driver life OS (Telegram + TurboVault plugins)

Docs map: [`README.md`](README.md) · open work: [`roadmap/current.md`](roadmap/current.md).

## 2026-07-23 update (engineering — read with 07-19)

- **Architecture hardening landed** on branch `architecture-hardening` (commit message: module splits, T1, MCP pooling):
  - **Module splits:** server API route groups, daemon lifecycle modules, config-loader model sections, executor budget module.
  - **MCP pooling (M1) + M1b degraded routing:** default-on pool via `tuning.mcp_pooling`; transport failure invalidates peers and marks them **degraded** on the shared catalog so `routing_descriptors()` omits them from dispatch. **Registry UX** (beyond hand-edited TOML) still open.
  - **T1 live conformance L1–L10:** L1–L8/L10 in `crates/server/src/t1_conformance.rs`; L9 in `liberado-daemon` (`l9_cron_event_becomes_joinable_dispatched_session`).
- Homelab **ops status** below is still the 2026-07-19 dogfood baseline unless you redeploy this branch.

## 2026-07-19 update (ops / dogfood)

- **Liberado is LIVE on the homelab** and is the chat surface for life ops:
  - API: `http://192.168.0.144:4201`
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
  then coding. See [`architecture/positioning.md`](architecture/positioning.md).
- **Doctrine:** [`architecture/failure-modes.md`](architecture/failure-modes.md) — live-verify
  every change against the real daemon.

**What's next is in [`roadmap/current.md`](roadmap/current.md).** Short version below.

---

## TL;DR — where it stands

| Surface | Status |
|---|---|
| Homelab daemon | **Live** — `liberado:dev`, compose under `~/homelab/services/liberado/` |
| Telegram chat + cron delivery | **Live** — free-form replies answer the sticky session; briefs fold in |
| TurboVault MCP peer | **Live** — `develop` image; vector module + tasks in agent reach |
| Provider | **Live** — OpenRouter / `deepseek/deepseek-v4-pro` |
| Vault mount | Liberado `:ro`; TurboVault peer `:rw` (agent writes via capability-gated tools) |
| OpenClaw briefings | **Retired** onto Liberado (no double-fire) |

### Operate it

```bash
ssh shiloh@homelab-node-ai
docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
docker logs liberado --tail 40
docker ps --filter name=liberado
curl -fsS http://192.168.0.144:4201/api/status
curl -fsS http://192.168.0.144:4201/api/models
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
| **API** | `http://192.168.0.144:4201` |
| **TurboVault peer** | `http://turbovault:3001` (nginx front; speak to this, not `turbovault-backend`) |

### TurboVault modules (sibling repo)

Work lives in the `turbovault/` sibling (not the life-os workspace). High level:

| Module | Branch / status | Liberado payoff |
|---|---|---|
| **Plugin API** (`turbovault-plugin-api`) | Landed (#39) | Boundary Liberado plugins use |
| **`vector`** | On fork `develop` (prototype Phases 1–4 done); live on homelab with `--features vector` | Semantic vault search from Telegram / briefs |
| **`tasks`** | `feat/plugin-tasks` (extraction + self-tuning + recurrence); core task tools also on `develop` | Life-OS todo surface; briefs already depend on tasks |
| **`vault_events`** | Planned — [`roadmap/turbovault-vault-events-plugin-plan.md`](roadmap/turbovault-vault-events-plugin-plan.md) | Optional L1 perception; not blocking Liberado P1 |

Umbrella: [`roadmap/turbovault-modules-integration-roadmap.md`](roadmap/turbovault-modules-integration-roadmap.md).

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
# Stream lean source from Windows repo (preserve remote turbovault develop clone):
#   tar + scp Cargo.toml Cargo.lock Dockerfile .dockerignore crates turbomcp config → ~/liberado-build
# TurboVault image is separate — rebuild with:
#   ~/homelab/scripts/rebuild-turbovault.sh develop
#   (enable module features in that Dockerfile/build: e.g. --features vector[,tasks])
# Liberado image:
ssh shiloh@homelab-node-ai 'cd ~/liberado-build && setsid bash -c "docker build -t liberado:dev . > ~/liberado-build.log 2>&1" </dev/null & disown'
# Poll: tail -f ~/liberado-build.log
# Then: docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
```

**Do not** use `pgrep -f "docker build"` alone — it self-matches the ssh command string.

---

## What's next for Liberado (priority order)

Strategy is still **daemon → chat → coding**. Modules and MCP breadth *support* the daemon daily-driver
bar; they do not replace it. Full table: [`roadmap/current.md`](roadmap/current.md).

### Priority 1 — daily-drive the autonomous life OS

1. **Dogfood Telegram.** Sticky free-form chat + cron delivery is the current phone surface and is
   good enough for the present use case. **Lean into using it** so real friction drives the next
   fixes — do not invent Telegram session multiplexing (operator call 2026-07-19). Known skip:
   `/model` catalog dump has no typeahead for model ids (Telegram only autocompletes top-level
   commands, not arguments).
2. **C1 remaining — interactive crons.** Delivery + OpenClaw cutover are done. Next is
   AskHuman-capable schedules ("run this every morning, **ask me if unsure**") via session profiles.
3. **M1b — MCP registry UX** (pooling + **degraded-catalog routing** landed 2026-07-23). Remaining:
   registration UX beyond hand-edited TOML only.
4. **T1 — Live conformance suite** ([`live-conformance-suite.md`](roadmap/live-conformance-suite.md)).
   **L1–L10 landed** (server suite + daemon L9). **Open:** Tier 2 only.
5. **W1 later — mobile WebUI session view.** Homespun browser UI when Telegram's flat chat is no
   longer enough. **Not** deep-linking background sessions into Telegram (E5-b deprioritized).

### TurboVault follow-through (not P1-blocking, high synergy)

- Land **`tasks`** module onto fork `develop` / upstream curation path (today: `feat/plugin-tasks`).
- Upstream-ready splits for **#42** (`plugin_state_dir`), **#43** (change-feed / `list_notes_meta`),
  and `vector` once Nick reviews shapes.
- **`vault_events`** module next in the modules sequence — consolidates perception; Liberado keeps
  L0 local watcher authoritative until L1 proves parity.
- Optional: turn-budget "battery" for briefs that hit the turn wall
  ([`ideas/turn-budget-battery-idea.md`](ideas/turn-budget-battery-idea.md)).

### Priority 2 / 3 (after the daily-driver bar)

- **CH1** WebUI chat maturity; **CH2** chat history search Tier 1 (lexical).
- Coding: resume mid-build (E6-c(b)), open vtcode no-write finding, eval curriculum — only when
  P1 is not the bottleneck.

**Move-on bar:** leave P1 when you **daily-drive without wincing**, not when it is polished.

---

## Orientation for a fresh agent

1. [`architecture/failure-modes.md`](architecture/failure-modes.md) — **live-verify doctrine**
2. [`architecture/overview.md`](architecture/overview.md)
3. [`architecture/sessions.md`](architecture/sessions.md)
4. [`architecture/positioning.md`](architecture/positioning.md)
5. [`roadmap/current.md`](roadmap/current.md) — open work in priority order
6. [`roadmap/turbovault-modules-integration-roadmap.md`](roadmap/turbovault-modules-integration-roadmap.md)
7. [`architecture/session-surface-contract.md`](architecture/session-surface-contract.md)

**Repo notes:** origin may lag local work; do not push unless asked. Local `turbovault` sibling is
co-developed — leave its branch alone unless the task is TurboVault work.

**Security:** never `docker compose config` with full auth.env in the output path. Liberado `.env` is
surgical on purpose.
