# MCP suite standardization — one brand, one library, one source of truth

**Status**: plan, 2026-07-16. Inventory is live-verified from `homelab-node-ai` (docker ps, compose
build contexts, container entrypoints, Cargo.toml of the local clones).

**Goal (operator's):** every custom MCP server is a **`liberado-*-mcp` repo on GitHub**, the homelab
**builds from that repo URL** (no local files), and every server uses **turbomcp** as its MCP library.

---

## The finding that matters

**None of the Rust MCPs use an MCP library — they hand-roll MCP over raw `axum`.**

That is the *root cause* of the connection chaos in
[`../../deploy/homelab/liberado-mcp-diagnosis.md`](../../deploy/homelab/liberado-mcp-diagnosis.md):
every server invented its own endpoint and auth, so Liberado (which always POSTs `/mcp`) hits 404/401.

| Server | Its MCP endpoint | Result |
|---|---|---|
| search-orchestrator | `/` (hand-rolled axum routes) | 404 |
| actual-mcp | `/http` | 404 |
| spider-mcp | `/mcp` but bearer-gated | 401 |
| searxng-mcp | binds `127.0.0.1` only | unreachable |

So **turbomcp standardization is not cosmetic — it is the fix.** One library ⇒ one endpoint, one
handshake. It also removes the need to add custom `path` / `bearer` support to Liberado's
`McpTransport::Http` (the alternative fix), and it honours the operator's veto on nginx sidecars.

---

## Pilot results — `liberado-search-orchestrator-mcp` (live-verified 2026-07-16)

The pilot is **done and verified on the homelab**: image builds, server boots, `POST /mcp`
handshake returns 200 with `serverInfo.name = liberado-search-orchestrator-mcp`, `tools/list`
returns both tools with auto-derived schemas, and `tools/call search_web` reaches the search core.
The original `/mcp` 404 and a newly-found 403 are both resolved. 29 tests pass, 0 warnings.

What the port cost, and the gotchas the next repo will hit — **read before porting the next one**:

- **turbomcp needs Rust 1.89.0+ and edition 2024.** The Dockerfile base must be `rust:1.89`+ (pilot
  uses `rust:1.94-bookworm`). The old `1.86` pin cannot compile the crate at all.
- **axum major version must match turbomcp's.** turbomcp is on **axum 0.8**; a crate still on 0.7
  gets incompatible `Router` types and `into_axum_router()` won't merge. Bump `axum = "0.8"` and
  `axum-test = "17.0"` together.
- **The 403 origin-validation trap (load-bearing).** turbomcp's `validate_origin_header` answers
  **403** to any non-loopback peer that sends no recognized `Origin`. Liberado is exactly that: a
  server-side client on the Docker bridge, no `Origin` header. Every request is refused unless you
  pass a permissive `ServerConfig`. The fix is an `origin_policy()` helper wired via
  `.with_config(...)` on the builder: default `allow_any_origin(true)`, with `MCP_ALLOWED_ORIGINS`
  (comma-separated) to switch to strict allow-listing if ever exposed to a browser. This is the same
  wall that forces the nginx Origin-rewrite in front of turbovault — fixed in-process instead, per
  the operator's veto on sidecars.
- **turbomcp serves `/`, `/mcp` and `/sse` natively** via `into_axum_router()`. Keep your REST
  routes (`/health`, `/metrics`, `/usage`, `/search`) and `.merge()` the turbomcp router — do not
  hand-roll any MCP endpoint. The pilot deleted ~480 lines of dispatcher + ~155 lines of protocol
  types (JsonRpc*, McpTool, InitializeResult, …) and the whole bespoke SSE transport.
- **Tool schemas are derived from `#[tool]` fn signatures** — `Option<T>` args become optional,
  everything else required. Never hand-write a schema again; drift between the list and the impl is
  the exact failure the old code allowed.
- **Rate-limit guard survives in degraded form.** turbomcp doesn't thread the peer address into
  tools, so the per-IP spend guard becomes a single shared bucket keyed on `127.0.0.1`. In practice
  identical — every MCP request already arrives from the one Liberado container.
- **Error mapping:** `McpError::tool_execution_failed(name, reason)` for core failures,
  `McpError::serialization(e)` for JSON, `McpError::rate_limited(msg)` for the guard.
- **Standalone-workspace footgun:** a crate extracted *inside* the life-os workspace needs its own
  `[workspace]` stanza in `Cargo.toml` or cargo refuses to build it.
- **Windows dev box:** cfg-gate `tokio::signal::unix` (`#[cfg(not(unix))] std::future::pending()`)
  or the binary won't compile on Windows, where it's developed.
- **turbomcp pin:** the fork `github.com/ForrestThump/turbomcp` is **public**; pin by `rev` to the
  tip of `fix/streamable-http-post-response-stream-hang` (carries both HTTP fixes). `Cargo.lock`
  then makes the container build reproducible.
- **Pre-existing repo bug:** `routing.yaml.example` was malformed YAML (3-space `searxng` block) —
  fixed before the push.
- **Private repo + BuildKit build (the deploy-side unlock).** The repos are **private**, and
  BuildKit's git-context fetch runs in the daemon and **ignores the host `~/.git-credentials`** — a
  bare `build: https://github.com/…` clone fails with "could not read Username". Fix without putting
  a token in the compose file or URL: a fine-grained PAT (Contents: read-only) at
  `~/homelab/secrets/github/pat`, declared as a top-level compose secret literally named
  `GIT_AUTH_TOKEN`, and listed under the service's `build.secrets: [GIT_AUTH_TOKEN]`. BuildKit
  consults that exact secret name to authenticate the git context. This is the standard for **every**
  private `liberado-*-mcp`. (Homelab git config already has `credential.helper store` for the PAT so
  host-side `git ls-remote` works too.)
- **Collapsed migrations vs an existing volume.** After squashing the migrations to one baseline, the
  persisted `usage.db` (in the project-prefixed volume, e.g. `compose_search-router-data`) still had
  the old granular versions recorded in `_sqlx_migrations`, so sqlx crash-looped on boot with
  `Migrate(VersionMissing(<old-version>))`. `db.rs` loads migrations at **runtime** from
  `./migrations` (= `/app/migrations`), not compile-time-embedded. Fix: rename the stale db aside
  (`mv usage.db usage.db.stale.<ts>`) so `mode=rwc` recreates it and the baseline applies clean.
  Non-destructive; keep the backup until verified. Watch for the **project-prefixed** volume name —
  the bare-named one is a decoy.
- **Log filter after rebrand:** the default `RUST_LOG` env-filter must target the new crate name
  (`liberado_search_orchestrator_mcp`), or the server's own INFO logs go silent post-rename.

---

## Inventory (live, 2026-07-16)

`✅` = already at target. `⚠️` = gap.

| Service (container) | Lang | MCP lib | Source repo | Builds from | Gaps |
|---|---|---|---|---|---|
| `liberado-weather-mcp` | Rust | TBD | `gh:ForrestThump/liberado-weather-mcp` | ✅ GitHub URL | lib TBD |
| `liberado-pdf-mcp` | Rust | TBD | `gh:…/liberado-pdf-mcp` | ✅ GitHub URL | lib TBD |
| `liberado-anythingllm-mcp` | Rust | TBD | `gh:…/liberado-anythingllm-mcp` | ✅ GitHub URL | lib TBD |
| `liberado-calorie-counter-mcp` | Rust | TBD | `gh:…/liberado-calorie-counter-mcp` | ✅ GitHub URL | ⚠️ binary is generic `liberado-mcp` |
| `caldav-mcp` | Rust | TBD | `gh:…/**liberado-**caldav-mcp` | ✅ GitHub URL | ⚠️ service name ≠ repo |
| `rentcast-mcp` | Rust | TBD | `gh:…/**liberado-**rentcast-mcp` | ✅ GitHub URL | ⚠️ name ≠ repo; **403** |
| `mem0-mcp` | Rust | TBD | `gh:…/**liberado-tool-helper-mcp**` | ✅ GitHub URL | ⚠️ **semantic drift**: repo says tool-helper, runs as mem0 |
| `actual-mcp` | **Node/TS** | JS MCP | `gh:…/liberado-actual-mcp` | ⚠️ **local** `../services/…` | ⚠️ name ≠ repo; **404 `/http`**; not Rust |
| `spider-mcp` | Rust | **axum (hand-rolled)** | ⚠️ **Gitea** `shiloh/spider-mcp` | ⚠️ local `../spider-mcp` | ⚠️ unbranded; Gitea; **401** |
| `search-orchestrator` | Rust | **axum (hand-rolled)** | ⚠️ **Gitea** `shiloh/search-orchestrator-mcp` | ⚠️ local | ⚠️ unbranded; Gitea; MCP at `/` |
| `subagent-manager-mcp` | Rust | TBD | ⚠️ **Gitea** `shiloh/subagent-manager-mcp` | not in ai.yml | ⚠️ unbranded; pkg is `context_manager_mcp` |
| `searxng-mcp` | TS | — | 3rd-party `isokoliuk/mcp-searxng` | pulled image | **drop** (superseded by search-orchestrator) |

**Gitea host:** `docker-server.mermaid-halfbeak.ts.net:3070` — also referenced as `192.168.4.10:3070`
and `100.100.213.44:3070`. Same server, three addresses: pick one canonical form.

---

## The good news

**7 of 11 already build directly from liberado-branded GitHub repos.** The "no local files" goal is
mostly achieved. The real remaining work is smaller than it looks.

### New GitHub repos needed — only 3

| Create | Migrate from (Gitea) |
|---|---|
| `liberado-spider-mcp` | `shiloh/spider-mcp` |
| `liberado-search-orchestrator-mcp` | `shiloh/search-orchestrator-mcp` |
| `liberado-subagent-manager-mcp` | `shiloh/subagent-manager-mcp` |

### Free wins (no repo work)

- **`actual-mcp` builds from a local clone but its GitHub repo already exists** — just point the
  compose `build:` at `https://github.com/ForrestThump/liberado-actual-mcp.git#master`.
- **Renames** (repo is already liberado-branded; only the compose service/container name drifts):
  `caldav-mcp` → `liberado-caldav-mcp`, `rentcast-mcp` → `liberado-rentcast-mcp`,
  `actual-mcp` → `liberado-actual-mcp`.
  ⚠️ Each rename changes the container **DNS name**, so Liberado's `topology.toml` url and the
  `ExecuteMcp` grant in `policy.toml` must change in the same step, or the MCP drops off.

---

## Decisions needed from the operator

1. **`mem0-mcp` vs `liberado-tool-helper-mcp`** — the repo and the role disagree. Rename the repo to
   `liberado-mem0-mcp`, or rename the service to match the repo? (What *is* it: mem0 bridge, or the
   generic tool-helper?)
2. **`actual-mcp` is Node/TS** — the only non-Rust server. Options: (a) rewrite in Rust/turbomcp
   (real work, fixes the `/http` 404 for free), (b) keep as the documented exception and fix only its
   path, (c) drop it. It is currently **failing** either way.
3. **`subagent-manager-mcp`** — package is `context_manager_mcp`. Which name is right? Is it still
   wanted?

---

## Migration recipe (per repo)

The operator's stated flow, made concrete:

```bash
# 1. Human: create empty GitHub repo `liberado-<x>-mcp` (no README — avoid a merge).
# 2. On the box (source of truth is the Gitea clone):
cd ~/homelab/<x>
git remote -v                                  # confirm gitea origin
git remote rename origin gitea                 # keep the old remote, don't lose it
git remote add origin git@github.com:ForrestThump/liberado-<x>-mcp.git
git push -u origin --all && git push origin --tags
# 3. Repo hygiene (in the repo): rename package/binary to liberado-<x>-mcp, swap MCP lib -> turbomcp.
# 4. Compose: build from the URL (private repo → GIT_AUTH_TOKEN build secret), rename svc/container:
#      build:
#        context: https://github.com/ForrestThump/liberado-<x>-mcp.git#master
#        secrets: [GIT_AUTH_TOKEN]        # top-level: secrets.GIT_AUTH_TOKEN.file = ../secrets/github/pat
#      container_name: liberado-<x>-mcp
# 5. Liberado config (SAME step as the rename): topology.toml url + policy.toml ExecuteMcp grant.
#    Edit the repo copies AND the deployed ~/homelab/services/liberado/config/ (keep them in sync).
# 6. Build + swap (operator's invocation, run from ~/homelab):
docker compose --env-file .env -f compose/docker-compose.ai.yml build liberado-<x>-mcp
docker rm -f <old-name>                                  # free the host port before up
docker compose --env-file .env -f compose/docker-compose.ai.yml up -d liberado-<x>-mcp
#    If it crash-loops on Migrate(VersionMissing): rename the stale db aside in the *project-prefixed*
#    volume (mv usage.db usage.db.stale.<ts>) so the collapsed baseline re-applies clean.
# 7. Verify live:
cd ~/homelab/services/liberado && docker compose up -d --force-recreate
docker logs liberado --tail 60 | grep -E "MCP failed|SSE connection established"
#    Then drive a REAL tool call (a query the model can't answer from memory) via POST :4201/api/chat
#    and confirm the executor log shows liberado-<x>-mcp:<tool> returning real results.
```

---

## Suggested order (pilot first, then repeat)

1. **Pilot: `search-orchestrator`** — ✅ **turbomcp swap done and live-verified 2026-07-16** (see
   "Pilot results" above). Remaining for this repo: fix `routing.yaml.example`, push to the new
   GitHub `liberado-search-orchestrator-mcp`, repoint compose `build:` at the URL, and update
   `topology.toml` + `policy.toml` in lockstep. Highest value (it is the *preferred* search MCP and
   the reason to drop searxng) and it exercised **every** step at once — the recipe survived, so the
   rest are repetition.
2. **`spider-mcp`** — ✅ **RESOLVED, but NOT via turbomcp.** The turbomcp swap **breaks scraping**
   (response-processing hang in the unified binary — not a version conflict, not fixable by feature
   toggles in one process). Full diagnosis + test matrix:
   [`../research/archive/spider-mcp-turbomcp-incompatibility.md`](../research/archive/spider-mcp-turbomcp-incompatibility.md).
   spider stays **hand-rolled**; the real "401" was an empty-`SPIDER_MCP_TOKEN` compose footgun, now
   fixed → Liberado connects to the working scraper.
3. **`subagent-manager-mcp`** — after the name decision.
4. **Free wins**: `actual-mcp` build-from-URL + the three renames (+ config in lockstep).
5. **turbomcp swap for the 7 already-on-GitHub repos** — one at a time, verify each live.
6. **Drop `searxng-mcp`** once search-orchestrator is connected.
7. **Decide `actual-mcp`** (rewrite / exception / drop).

**Do not batch these.** Each step changes a container DNS name or an endpoint; the failure-modes
doctrine applies — one MCP at a time, `docker logs liberado` after each, and confirm no
`MCP failed to connect`. Verify with a real tool call where possible (weather already proved the
end-to-end path via `delegate`).
