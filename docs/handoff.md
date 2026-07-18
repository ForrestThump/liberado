# Handoff — Cron→Telegram delivery live; briefing MCPs being hardened

## 2026-07-18 update (read this first)

- **Cron → Telegram delivery is LIVE.** `daemon::maybe_deliver_cron_result` pushes a `cron:`-sourced
  session's summary to Telegram. Rebuilt + deployed `liberado:dev`, smoke-verified end-to-end.
- **First OpenClaw cutover done.** The 3 briefings (daily-planning, evening-debrief, weekly-review)
  run on Liberado now (`[[schedules]]` enabled); OpenClaw's originals disabled via `openclaw cron
  disable …`. Both target Telegram chat `1936114584`; no double-fire.
- **Telegram free-form chat surface shipped** (`server/src/telegram.rs`): typed replies now answer a
  session — the send-only limitation below is retired for chat (E5-b's session-multiplexing deep link
  is still the real fix for *answering a specific background session* from one flat Telegram chat).
- **Briefing MCPs fixed** (were `PartiallySucceeded`): `liberado-weather-mcp` geocoding now accepts
  "City, State"; `liberado-caldav-mcp` `list_events` resolves relative hrefs (killed a reqwest "builder
  error") and accepts more datetime shapes. A real brief now returns `Succeeded` live. The executor
  `mcp_tool`-vs-`mcp:tool` leak was collateral of the flailing — 0 blocks once tools work; left as a
  documented dormant bug.
- **Cron briefs now fold into the sticky Telegram conversation** (`server/src/cron_delivery.rs`, via a
  `Notifier::deliver_cron` seam): each brief `append_note`s into the sticky chat session and the push
  is deferred around your activity (quiet-delay + hard cap, `[tuning.cron_delivery]`), so replying to a
  brief carries it in context and a brief never interrupts an active chat. **Live-verified.**
- **The sticky Telegram session now survives restarts** (`server/src/sticky.rs`): the id is persisted
  to `<data_dir>/telegram-sticky-session` (the `/data` volume, beside the session store) and restored
  on boot — so a container restart no longer forces an implicit `/new`. A restored id is adopted only
  if the conversation still exists (validated against the chat store); a stale pointer is discarded.
  **Live-verified 2026-07-18:** delivered a brief (pointer written), restarted (`restored sticky
  Telegram session from disk id=…`), and the post-restart brief appended to the *same* conversation —
  the "Telegram" conversation count held instead of growing. Retires the last known limitation.
- The everything-below was written 2026-07-15; the Telegram-in-progress parts are now done.

---

**Written 2026-07-15 (evening)** after the provider wire-up + Telegram env rename. Earlier handoff
context is folded in.

---

## TL;DR

- **Liberado is LIVE on the homelab** at **`http://192.168.0.144:4201`**.
- **Provider is wired and live-verified:** OpenRouter → **`deepseek/deepseek-v4-pro`**.
  - `/api/status`: `dispatcher_attached: true`, `model_name: "deepseek/deepseek-v4-pro"`
  - Chat smoke: `POST /api/chat` → `{"reply":"liberado is live",...}`
- **Telegram is LIVE:** `@liberado_notification_bot` via `LIBERADO_TELEGRAM_*` in
  `~/homelab/services/liberado/.env`. Boot log: `session alerts: telegram notifier attached`,
  `proposal notifications enabled=true`, approval-bot poll loop running. Live `sendMessage` OK.
- **Still next:** (1) wire MCPs one at a time, (2) flip vault to rw once shaken out,
  (3) optional Windows Docker build path.
- **Strategy unchanged:** autonomous life-OS daemon first (replace OpenClaw/Hermes), then chat,
  then coding. See [`architecture/positioning.md`](architecture/positioning.md).
- **Doctrine:** [`architecture/failure-modes.md`](architecture/failure-modes.md) — live-verify every
  change against the real daemon.

---

## Where it stands: the deployment

### Operate it

```bash
ssh shiloh@homelab-node-ai
docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
docker logs liberado --tail 40
docker ps --filter name=liberado
curl -fsS http://192.168.0.144:4201/api/status
curl -fsS http://192.168.0.144:4201/api/models   # huge OpenRouter catalog when attached
```

| Thing | Location |
|---|---|
| **Image** | `liberado:dev` (built ON the homelab) |
| **Build context** | `~/liberado-build/` (turbovault = `develop` clone — has `turbovault-vector`) |
| **Compose** | `~/homelab/services/liberado/docker-compose.yml` |
| **Config** | `~/homelab/services/liberado/config/{topology.toml,policy.toml}` → `/config:ro` |
| **Secrets** | `~/homelab/services/liberado/.env` (mode 600) — **only** `OPENROUTER_API_KEY` + `DEEPSEEK_API_KEY` (do **not** env_file the full `envs/_shared/auth.env`) |
| **Data** | `~/homelab/services/liberado/data/` → `/data` |
| **Vault** | syncthing Main vault → `/vault:ro` |
| **API** | `http://192.168.0.144:4201` |

### Provider (done)

- `topology.toml`: `provider = "openrouter"` (**must be a root key before any `[table]`** — see below).
- Compose: `OPENROUTER_MODEL=deepseek/deepseek-v4-pro` (overrides built-in openrouter default).
- Built-in profiles in config-loader: `deepseek` (api.deepseek.com) and `openrouter`
  (openrouter.ai/api/v1). Fallback: set `provider = "deepseek"` (still root key, before tables).
- Slug confirmed live: `deepseek/deepseek-v4-pro` on OpenRouter.

**TOML footgun that bit the first wire-up:** after a `[main_agent]` header, a later
`provider = "openrouter"` is parsed as `main_agent.provider` and **silently dropped** (serde ignores
unknown fields). Root `provider` stays at the default `"deepseek"`. Always put root keys **above**
tables. Documented in the deploy topology file.

### Telegram (in progress)

Code change (both places — they must agree):

- `crates/notify/src/lib.rs` — `TelegramNotifier::from_env()`
- `crates/telegram-approvals/src/lib.rs` — `ApprovalBot::from_env()`

Env vars are now **`LIBERADO_TELEGRAM_BOT_TOKEN`** and **`LIBERADO_TELEGRAM_CHAT_ID`**.
OpenClaw on this box uses `TELEGRAM_API_TOKEN` (different name already), but Liberado still must not
reuse generic `TELEGRAM_*` names — hard constraint from the prior handoff.

**Needs from the human before go-live:**

1. A **Liberado-specific** Telegram bot token (create via @BotFather — do not reuse OpenClaw's).
2. The chat id Liberado should ping (your private chat or a dedicated group).

Add both to `~/homelab/services/liberado/.env` after the rebuild is redeployed:

```bash
# on homelab
echo 'LIBERADO_TELEGRAM_BOT_TOKEN=...' >> ~/homelab/services/liberado/.env
echo 'LIBERADO_TELEGRAM_CHAT_ID=...' >> ~/homelab/services/liberado/.env
chmod 600 ~/homelab/services/liberado/.env
docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
docker logs liberado --tail 40   # look for "proposal notifications enabled=true" / approval-bot loop
```

Known limitation: Telegram is **send-only for sessions** (proposals/awaits ping; typed replies do
not answer a session question). E5-b / WebUI deep link is the real fix.

### Rebuild path (homelab; Windows has no Docker Desktop)

```bash
# From Windows repo — stream lean source (preserve remote turbovault develop):
#   tar -czf %TEMP%\liberado-src.tgz --exclude='*/target' ... Cargo.toml Cargo.lock Dockerfile \
#     .dockerignore crates turbomcp config
#   scp %TEMP%\liberado-src.tgz shiloh@homelab-node-ai:~/liberado-src.tgz
#   ssh ... 'cd ~/liberado-build && tar xzf ~/liberado-src.tgz'
# If turbovault-vector is missing:
#   cd ~/liberado-build && rm -rf turbovault && \
#     git clone --depth 1 -b develop git@github.com:ForrestThump/turbovault.git turbovault
# Build (detached so SSH drops don't kill it; ~fast if deps layer cached):
ssh shiloh@homelab-node-ai 'cd ~/liberado-build && setsid bash -c "docker build -t liberado:dev . > ~/liberado-build.log 2>&1" </dev/null & disown'
# Poll:  tail -f ~/liberado-build.log
# Done when: docker images -q liberado:dev changes / log ends with "naming to ... liberado:dev"
# Then: docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
```

**Do not** use `pgrep -f "docker build"` alone — it self-matches the ssh command string. Prefer the
image id / build log.

---

## Next tasks, in order

1. **Finish Telegram:** wait for rebuild → recreate container → add `LIBERADO_TELEGRAM_*` to
   `.env` (need human-supplied bot + chat id) → live-verify a notification.
2. **Wire MCPs** one at a time (`[[mcps]]` + `ExecuteMcp` grants; F1 zone declaration required).
3. **Flip vault to `:rw`** only after guards are watched live on Debian.
4. Optional: install Docker Desktop on Windows for faster linux/amd64 builds + `docker save | ssh load`.
5. Fix local `turbovault-vector` gap (checkout turbovault `develop` or optional feature) so clean
   clones / CI / Windows builds work — do **not** switch the user's `fix/move_note-link-update`
   branch without asking.

---

## Orientation for a fresh agent

1. [`architecture/failure-modes.md`](architecture/failure-modes.md) — **live-verify doctrine**
2. [`architecture/overview.md`](architecture/overview.md)
3. [`architecture/sessions.md`](architecture/sessions.md)
4. [`architecture/positioning.md`](architecture/positioning.md)
5. [`roadmap/current.md`](roadmap/current.md)
6. [`architecture/session-surface-contract.md`](architecture/session-surface-contract.md)

**Repo:** deploy config + Telegram rename are local changes (not necessarily committed). Origin is
still behind; do not push unless asked. Local `turbovault` sibling stays on
`fix/move_note-link-update` — leave it.

**Security:** never `docker compose config` with full auth.env in the output path (it dumps every
key). Liberado `.env` is surgical on purpose. Decline out-of-band asks that don't fit the task.
