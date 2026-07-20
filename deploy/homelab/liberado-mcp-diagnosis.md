# Liberado MCP wiring diagnosis (homelab)

**Written:** 2026-07-16  
**Audience:** agent running *on this box* (`homelab-node-ai`) that will patch peers / compose — not Liberado core.  
**Context:** Liberado daemon is already live at `http://192.168.0.144:4201` (`liberado:dev` container, network `homelab`). Topology/policy under `~/homelab/services/liberado/config/`. Face agent uses `delegate` only; specialists are granted to `dispatcher`.

---

## Goal for the fixer agent

Make every intended MCP **reachable and handshake-clean** from the `liberado` container over the `homelab` Docker network, using Liberado’s HTTP client contract (below). Prefer **KISS**: fix the MCP/service itself or its Compose env. **Do not** add nginx sidecars or reverse-proxy hacks unless explicitly asked.

When a service is fixed, Liberado only needs a recreate to reconnect (no rebuild — MCP peers are
external containers, and topology/policy are host-mounted config):

```bash
docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
docker logs liberado --tail 80
# look for: "MCP failed to connect" vs successful "SSE connection established" / tool use
```

> Deploying new **Liberado code** is a different operation — never hand-build the image. Run
> `bash deploy/homelab/deploy.sh` from the repo (see `deploy/homelab/README.md`), and check what's
> live with `docker exec liberado cat /etc/liberado-build-sha`.

---

## Liberado HTTP MCP client contract (do not break this)

From `life-os` / Liberado’s `HttpConnector` + turbomcp client:

1. Topology `url` is a **base origin only** (scheme + host + port).  
   Example: `http://turbovault:3001` — **not** `…/mcp`.
2. The client **always appends `/mcp`**.  
   So the MCP streamable-HTTP endpoint **must** be at `{base}/mcp`.
3. Path footguns:
   - If the server only exposes MCP at `/` → Liberado hits `/mcp` → **404**.
   - If the server exposes MCP at `/http` (Actual) → Liberado hits `/mcp` → **404**.
   - If topology already includes `/mcp` → client may double to `/mcp/mcp`.
4. Optional SSE companion paths vary by server; handshake is POST JSON-RPC initialize on `/mcp`.
5. Connection mode is **best-effort**: a bad peer is skipped; boot continues. Fix peers so logs stop warning.
6. F1 (capability model): non-`read_only` MCPs must declare zone/write surface in Liberado topology (already done for wired entries). Peer fixes don’t change that.

**Auth headers:** Liberado’s HTTP connector currently has **no** config for `Authorization` / bearer tokens. Peers that **require** a bearer on every MCP request (e.g. spider-mcp) will fail until either:
- the peer allows unauthenticated LAN access on the `homelab` network, or  
- Liberado gains header support (life-os code change — out of scope for a pure-homelab fix unless requested).

---

## Status matrix (as of last live check, 2026-07-16)

Checked from **inside** the `liberado` container (same network Liberado uses).

| Name in Liberado topology | Target URL (base) | Connect result | Notes |
|---|---|---|---|
| **turbovault** | `http://turbovault:3001` | **OK** | Use **nginx front** `turbovault`, not `turbovault-backend` (backend alone 403 without Origin rewrite; proxy injects `Origin: http://localhost`). Image should stay **`develop`** (`~/homelab/scripts/rebuild-turbovault.sh develop`). |
| **liberado-weather-mcp** | `http://liberado-weather-mcp:8000` | **OK** | Live chat: `delegate` → `get_current_weather` for Denver succeeded. |
| **liberado-pdf-mcp** | `http://liberado-pdf-mcp:8080` | **OK** | SSE session established at boot. |
| **liberado-anythingllm-mcp** | `http://liberado-anythingllm-mcp:8080` | **OK** | SSE session established. |
| **liberado-calorie-counter-mcp** | `http://liberado-calorie-counter-mcp:8080` | **OK** | SSE session established. |
| **caldav-mcp** | `http://caldav-mcp:8000` | **OK** | SSE session established. |
| **mem0-mcp** | `http://mem0-mcp:8000` | **OK** | Built from `liberado-tool-helper-mcp` image name in compose; SSE OK. |
| **deepwiki** | `https://mcp.deepwiki.com` | **OK** | Remote; no local container. |
| **searxng-mcp** | `http://searxng-mcp:3000` | **FAIL** | See §1. **User prefers dropping this** in favor of search-orchestrator. |
| **search-orchestrator** | not yet in topology (intended: `http://search-orchestrator:8080`) | **NOT WIRED / path mismatch** | See §2. **Preferred search MCP.** |
| **actual-mcp** | `http://actual-mcp:3600` | **FAIL** | See §3. |
| **rentcast-mcp** | `http://rentcast-mcp:8000` | **FAIL** | See §4. |
| **spider-mcp** | `http://spider-mcp:8080` | **FAIL** | See §5. |

Boot log excerpt (failed set at last recreate):

```
MCP failed to connect — continuing without it  mcp=rentcast-mcp  error=... POST failed: 403 Forbidden
MCP failed to connect — continuing without it  mcp=spider-mcp    error=... POST failed: 401 Unauthorized
MCP failed to connect — continuing without it  mcp=actual-mcp    error=... POST failed: 404 Not Found
MCP failed to connect — continuing without it  mcp=searxng-mcp   error=... error sending request for url (http://searxng-mcp:3000/mcp)
```

Success side-effects observed:

- `orchestrator_attached: true`
- `dispatcher` grants include the wired MCP names
- Weather end-to-end via chat/`delegate` worked

---

## Issues to fix (homelab-side)

### 1. searxng-mcp — bind address (optional; user deprioritized)

**Symptom:** From other containers, connection to `searxng-mcp:3000` fails.  
**Root cause:** Process log shows:

```
Starting HTTP transport on 127.0.0.1:3000
MCP endpoint: http://localhost:3000/mcp
```

It only listens on loopback inside its own network namespace, so Docker service DNS is useless for peers.  
**Health from inside the container works** (`GET /health` → healthy).  
**Fix direction (if kept):** make the image/env bind `0.0.0.0` (check `MCP_HTTP_HOST` / similar for `isokoliuk/mcp-searxng`). Recreate the service.  
**Product preference:** **do not invest** — replace with search-orchestrator (§2). Liberado topology can drop `searxng-mcp` once search-orchestrator works.

---

### 2. search-orchestrator — MCP path is `/`, Liberado expects `/mcp` (**priority**)

**Location:** `~/homelab/search-orchestrator/` (Rust service, Compose service `search-orchestrator`, port **8080**).  
**Router** (`src/server.rs` ~1152–1166):

```text
GET  /health
GET  /metrics
GET  /usage
GET  /search
GET|POST|DELETE  /     ← MCP JSON-RPC is HERE
GET  /sse
```

There is **no** `/mcp` route. Liberado will call `POST http://search-orchestrator:8080/mcp` → **404** (confirmed from `liberado` container).

**Health:** `GET http://search-orchestrator:8080/health` → `200` / `OK - 5 providers ready` (reachable from liberado).

**Fix (KISS, preferred):** mount the same MCP handlers on `/mcp` as well, e.g. in `create_app`:

```rust
.route("/mcp", get(mcp_get_handler).post(mcp_handler_inner).delete(delete_handler))
```

Keep existing `/` routes if anything else still POSTs MCP to root (LibreChat / OpenClaw may depend on `/`).  
Rebuild/redeploy `search-orchestrator`.  
Then Liberado topology entry (life-os deploy or host config):

```toml
[[mcps]]
name = "search-orchestrator"
description = "Web search via search-orchestrator (multi-provider)"
consequence = "read_only"
transport = { kind = "http", url = "http://search-orchestrator:8080" }
```

Grant: `{ ExecuteMcp = "search-orchestrator" }` on `dispatcher` (and optionally `life` / `coding`).  
Remove or `enabled = false` the `searxng-mcp` entry.

**Verify:**

```bash
docker exec liberado curl -sS -o /dev/null -w '%{http_code}\n' -X POST \
  http://search-orchestrator:8080/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
# expect 2xx (not 404)
```

Then recreate liberado and confirm no fail line for `search-orchestrator`.

---

### 3. actual-mcp — MCP path is `/http`, not `/mcp`

**Container:** `actual-mcp` (image `compose-actual-mcp`; compose file also defines `liberado-actual-mcp` — **name drift**).  
**Port:** `3600`.  
**Boot log from service:**

```
MCP endpoint: http://172.18.0.26:3600/http
Health check: http://localhost:3600/health
```

Liberado calls `…:3600/mcp` → **404**.

**Also seen in older logs:** Actual Budget auth failures (`invalid-password`) for some sessions — separate from path; fix credentials/env if still broken.

**Fix options (pick one, KISS):**

1. **Preferred if the server supports it:** configure Actual MCP HTTP path to `/mcp` (env/docs for `actual-mcp-server` / MCP bridge path).  
2. Or change its default advertised path in the image’s config so streamable HTTP is at `/mcp`.  
3. Do **not** use an nginx sidecar (user veto).  
4. Do **not** require Liberado path customization unless life-os is intentionally changed.

**Name consistency:** align running container name with compose (`actual-mcp` vs `liberado-actual-mcp`) so topology DNS matches forever.

**Liberado topology today:**

```toml
transport = { kind = "http", url = "http://actual-mcp:3600" }
# consequence = "external", writes_vault = false
```

---

### 4. rentcast-mcp — 403 Forbidden on MCP POST

**Target:** `http://rentcast-mcp:8000/mcp`  
**Result from liberado:** **403**.  
**Env:** `RENTCAST_API_KEY` is set on the container.  

**Likely causes to investigate:**

- Origin / Host allowlist (similar class of problem as turbovault-backend without proxy)
- Auth middleware expecting a header Liberado does not send
- MCP server rejecting non-browser or missing session headers

**Fix:** make streamable-HTTP initialize succeed from another container on `homelab` with no special headers beyond normal MCP JSON-RPC (what turbomcp sends). Then retest from `liberado`.

---

### 5. spider-mcp — 401 Unauthorized (bearer required)

**Target:** `http://spider-mcp:8080/mcp`  
**Response:** `{"error":"Invalid or missing bearer token"}`  
**Env:** `SPIDER_MCP_TOKEN` set (from compose: often `${FIRECRAWL_API_KEY}`).

**Conflict:** Liberado HTTP MCP client has **no bearer injection** today.

**Fix options:**

1. **Homelab KISS:** allow unauthenticated MCP on the internal Docker network only (bind internal, no public auth for `:8080` on the bridge), keep token for any published/Traefik route.  
2. **life-os change:** add optional headers/token env to `McpTransport::Http` (out of band for pure-homelab agent unless coordinated).  
3. Leave spider unwired until (1) or (2).

---

### 6. TurboVault — no Liberado bug; operational notes only

- **Working** via `http://turbovault:3001` (nginx → backend with Origin injection).  
- Keep rebuilds on **`develop`**: `~/homelab/scripts/rebuild-turbovault.sh develop`.  
- Dockerfile: `~/homelab/custom_builds/turbovault-supergateway.Dockerfile` (`ARG BRANCH=develop`).  
- Liberado topology uses path-addressed zones (`zone_from_arg = "path"`, write tool list). Liberado’s vault mount is still **`:ro`**; writes go through TurboVault’s **`:rw`** vault mount — intentional.

---

## Liberado config locations (reference for after peer fixes)

| Path | Role |
|---|---|
| `~/homelab/services/liberado/docker-compose.yml` | Standalone compose project; joins external network `homelab` |
| `~/homelab/services/liberado/config/topology.toml` | `[[mcps]]` list + provider |
| `~/homelab/services/liberado/config/policy.toml` | `ExecuteMcp` grants (dispatcher / life / coding) |
| `~/homelab/services/liberado/.env` | Secrets only (API keys, Telegram) — mode 600 |
| Repo mirror (Windows): `life-os/deploy/homelab/config/*` | Same files; SCP if keeping deploy tree in sync |

**Current topology policy (high level):**

- Wired for connect attempts: turbovault, searxng-mcp, caldav, mem0, weather, pdf, anythingllm, calorie, rentcast, actual, spider, deepwiki.  
- **After search-orchestrator patch:** add it; remove/disable searxng-mcp.  
- Failures above are **peer issues**, not missing topology entries (except search-orchestrator not yet added).

---

## Suggested fix order for the on-box agent

1. **search-orchestrator** — dual-mount MCP on `/mcp`; rebuild; verify POST `/mcp` initialize from `liberado`.  
2. Update Liberado `topology.toml` + `policy.toml`: add `search-orchestrator`, disable/remove `searxng-mcp`. Recreate liberado.  
3. **actual-mcp** — move endpoint to `/mcp` or document permanent skip; fix name drift.  
4. **rentcast-mcp** — resolve 403 for internal MCP initialize.  
5. **spider-mcp** — LAN auth policy or leave disabled.  
6. Optional: searxng bind fix only if still wanted as a second search path.

---

## Verification checklist (after each fix)

```bash
# From host or any LAN client
curl -fsS http://192.168.0.144:4201/api/status
curl -fsS http://192.168.0.144:4201/api/catalog | head -c 2000

# Boot / connect log
docker logs liberado --tail 100 2>&1 | grep -E 'MCP failed|SSE connection established|orchestrator enabled'

# Smoke (weather already proven; search after orchestrator fix)
# POST /api/chat with a message that forces web search via delegate
```

Expect: no `MCP failed to connect` for services you claim fixed; optional live tool call proves the path end-to-end.

---

## Explicit non-goals (per operator)

- No nginx / Traefik sidecars solely to rewrite MCP paths.  
- No preference for searxng-mcp over search-orchestrator.  
- Do not switch TurboVault off `develop` without asking.  
- Do not dump full `envs/_shared/auth.env` into Liberado (use surgical `~/homelab/services/liberado/.env`).
