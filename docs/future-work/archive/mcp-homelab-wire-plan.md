# Homelab MCP wiring plan â€” connect first, forge for source discipline

**Status**: largely executed (2026-07-19). Homelab topology wires TurboVault (with live
`vector` / tasks reach from Liberado), weather, CalDAV, search-orchestrator, qdrant,
spider, actual, and others â€” see `deploy/homelab/config/topology.toml`. Goal was: give
Liberado the MCP breadth it needs to replace OpenClaw, with the smallest possible
footprint on the already-running stack. Remaining work is M1 (pool/registry UX) in
[`current.md`](../../roadmap.md), not greenfield peer wiring.

**Doctrine**: live-verify every MCP one at a time (`/api/status` + logs + a real tool call).
F1: every non-`read_only` MCP must declare what it writes or the daemon refuses to boot.

---

## The key split (do not collapse these)

| Layer | What it is | Role on the **homelab** | Role on a **dev machine** |
|---|---|---|---|
| **`liberado-mcp-forge`** | Offline CLI: `cargo install --git` / `--path` â†’ binary under the managed install dir | **Not the runtime path.** Forge explicitly does *not* supervise long-running HTTP servers (see `crates/mcp-forge/ARCHITECTURE.md` non-goals). | Optional: rebuild Liberado-branded stdio binaries for local `McpTransport::Managed` |
| **Homelab Compose** (`compose/docker-compose.ai.yml`) | Already builds most Liberado MCPs **from GitHub** and runs them as HTTP containers on the `homelab` bridge | **Source of truth for process lifecycle** (restart, health, ports, secrets) | N/A |
| **Liberado daemon** | `topology.toml` `[[mcps]]` + `policy.toml` `ExecuteMcp` | **Connects** as an MCP *client* over HTTP to neighbors by container DNS name | Same shape, different URLs (`127.0.0.1`) |

**Recommendation:** on the homelab, treat forge as documentation of *where source lives*
(same git URLs Compose already uses), not as something Liberado runs at boot. The
small-footprint path is **config-only HTTP wiring** into containers that already exist.

```
  â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ life-os repo â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
  â”‚  mcp-forge  â”€â”€syncâ”€â”€â–º  managed binaries       â”‚  (local/dev; optional)
  â”‚  topology  [[mcps]]  â”€â”€â–º  HttpConnector URLs  â”‚  (homelab: container DNS)
  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
                         â”‚
                         â–¼  HTTP on `homelab` network
  â”Œâ”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€ already running â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”
  â”‚  turbovault(:3001)  weather  caldav  mem0     â”‚
  â”‚  pdf  rentcast  anythingllm  calorie  actual  â”‚
  â”‚  searxng-mcp  spider-mcp  â€¦                   â”‚
  â”‚  images built from github.com/ForrestThump/*  â”‚
  â””â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”˜
```

---

## What is already live (inventory, 2026-07-16)

Liberado resolves all of these by name from inside its container (shared `homelab` network).

| Container | Image / build | Internal listen | Consequence (proposed) | Zone declaration (F1) |
|---|---|---|---|---|
| **turbovault** (nginx) â†’ **turbovault-backend** | Custom build from `ForrestThump/turbovault` **`BRANCH=develop`** (Dockerfile default) | `3001` (proxy injects `Origin: http://localhost` for turbomcp allowlist) | `reversible` | `zone_from_arg = "path"` + write_tools list (already in repo template) |
| **liberado-weather-mcp** | `github.com/ForrestThump/liberado-weather-mcp#master` | `8000` | `read_only` | none needed |
| **liberado-pdf-mcp** | `â€¦/liberado-pdf-mcp#master` (FEATURES=native-ocr) | `8080` | `reversible` | `writes_vault = false` |
| **liberado-anythingllm-mcp** | `â€¦/liberado-anythingllm-mcp#master` | `8080` | `reversible` | `writes_vault = false` |
| **liberado-calorie-counter-mcp** | `â€¦/liberado-calorie-counter-mcp#master` | `8080` | `reversible` | `writes_vault = false` (own DB) |
| **caldav-mcp** | `â€¦/liberado-caldav-mcp#master` | `8000` | `reversible` | `writes_vault = false` (calendar server) |
| **rentcast-mcp** | `â€¦/liberado-rentcast-mcp#master` | `8000` | `read_only` | none |
| **mem0-mcp** | `â€¦/liberado-tool-helper-mcp#master` (compose name is historical) | `8000` | `reversible` | `writes_vault = false` (mem0 store) |
| **searxng-mcp** | `isokoliuk/mcp-searxng:latest` (third-party) | `3000` | `read_only` | none |
| **actual-mcp** / liberado-actual | compose now defines `liberado-actual-mcp`; running name may still be `actual-mcp` | `3600` or `8000` | `external` or `reversible` | `writes_vault = false` (finance system) |
| **spider-mcp** | local `homelab/spider-mcp` build | `8080` | `external` (web scrape) | `writes_vault = false` |
| **search-orchestrator** | local build | `8080` | TBD / maybe skip | â€” |
| **deepwiki** | remote SaaS | `https://mcp.deepwiki.com` | `read_only` | none (no container) |

Bare `GET /mcp` often returns 400 â€” that is normal; streamable-HTTP MCP wants a proper
initialize handshake. DNS from liberado is green for the core set.

### TurboVault specifics (non-negotiable: `develop`)

- Homelab rebuild script: `~/homelab/scripts/rebuild-turbovault.sh <branch>` (default docs say `develop`).
- Dockerfile: `ARG BRANCH=develop` + `git clone --branch ${BRANCH} git@github.com:ForrestThump/turbovault`.
- Liberado must speak to **`http://turbovault:3001`** (nginx proxy), **not**
  `http://turbovault-backend:3001` (backend alone 403s without the Origin rewrite).
- `url` is **base origin only** â€” client appends `/mcp` (same footgun as deepwiki).
- Vault: Liberado mounts vault **read-only**; turbovault-backend mounts it **read-write**.
  That is the right F1 shape for v1: agent writes go through capability-gated TurboVault tools,
  not a raw filesystem open from the daemon process.
- Zone model (already designed in `config/topology.toml`):

  ```toml
  zone_from_arg = "path"
  write_tools = ["write_note", "delete_note", "move_note", â€¦]
  ```

Do **not** rebuild TurboVault via mcp-forge for the daemon. The running service *is* the
integration surface; keep its image on `develop` via the existing homelab script.

---

## How forge fits (without fighting Docker)

### What forge is good for

1. **Local Windows/Linux dev** â€” `liberado-mcp-forge sync` installs Liberado-branded servers as
   managed stdio binaries so a laptop daemon can use `transport = { kind = "managed" }` without
   Docker Desktop.
2. **Source registry** â€” `mcp-sources.toml` is the single list of â€œwhere does this MCPâ€™s code live,â€
   preferably the same GitHub URLs Compose already uses.
3. **New MCPs that are not yet containerized** (e.g. `liberado-wakeup-mcp` until it has a compose
   service) â€” forge â†’ managed binary, or add a GitHub-context build to `docker-compose.ai.yml`.

### What forge is *not* for (on the homelab)

- Spawning stdio children inside the `liberado` container for servers that already run as
  long-lived HTTP peers (duplicates process, loses Compose healthchecks, doubles RAM).
- Building TurboVault (heavy; already specialized with Origin proxy + vault mount).
- Replacing Composeâ€™s `build: https://github.com/â€¦#branch` pipeline.

### Alignment rule

```
GitHub repo (ForrestThump/* or turbovault develop)
        â”‚
        â”œâ”€â–º Homelab Compose  â”€build imageâ”€â–º long-running HTTP container
        â”‚                                      â–²
        â”‚                                      â”‚ HttpConnector
        â”‚                                   liberado (topology)
        â”‚
        â””â”€â–º mcp-sources.toml â”€forge syncâ”€â–º managed binary (dev only)
```

Same git URL in both places when the MCP is Liberado-branded. TurboVault is Compose-only
(`develop`), not an `mcp-sources` managed entry for the daemon.

---

## Recommended topology shape (homelab)

Illustrative â€” wire **one block at a time**, recreate, live-verify, then next.

```toml
# deploy/homelab/config/topology.toml  (root keys before any [table])

vault_path = "/vault"
provider = "openrouter"

[main_agent]
delegation_mode = true

# 1) TurboVault â€” path-addressed vault ops (develop image via existing compose)
[[mcps]]
name = "turbovault"
description = "Obsidian vault ops: notes, search, links, templates, batch edits"
consequence = "reversible"
transport = { kind = "http", url = "http://turbovault:3001" }
zone_from_arg = "path"
write_tools = [
    "write_note", "delete_note", "move_note", "move_file",
    "update_frontmatter", "create_from_template", "update_task", "delete_task",
]

# 2) Web search
[[mcps]]
name = "searxng-mcp"
description = "Web search via local SearXNG"
consequence = "read_only"
transport = { kind = "http", url = "http://searxng-mcp:3000" }

# 3) Calendar
[[mcps]]
name = "caldav-mcp"
description = "Calendar read/write via CalDAV"
consequence = "reversible"
transport = { kind = "http", url = "http://caldav-mcp:8000" }
writes_vault = false

# 4) Weather
[[mcps]]
name = "liberado-weather-mcp"
description = "Current weather and forecast"
consequence = "read_only"
transport = { kind = "http", url = "http://liberado-weather-mcp:8000" }

# 5) Memory (mem0)
[[mcps]]
name = "mem0-mcp"
description = "Long-term memory store (mem0)"
consequence = "reversible"
transport = { kind = "http", url = "http://mem0-mcp:8000" }
writes_vault = false

# 6+) pdf, rentcast, calorie, anythingllm, actual â€” same HTTP pattern
# Remote:
# [[mcps]]
# name = "deepwiki"
# consequence = "read_only"
# transport = { kind = "http", url = "https://mcp.deepwiki.com" }
```

Matching **policy** (dispatcher grant only â€” face stays thin via `delegate`):

```toml
[[grants]]
component = "dispatcher"
capabilities = [
    # existing Read/Write zonesâ€¦
    { ExecuteMcp = "turbovault" },
    { ExecuteMcp = "searxng-mcp" },
    { ExecuteMcp = "caldav-mcp" },
    { ExecuteMcp = "liberado-weather-mcp" },
    { ExecuteMcp = "mem0-mcp" },
    # â€¦
]
```

`life` / `coding` grants: add only the MCPs those packs should hold *without* going through
dispatch (usually leave specialists on `dispatcher`).

---

## Rollout order (smallest risk â†’ broadest capability)

| Step | MCP | Why this order | Verify |
|---|---|---|---|
| **0** | Confirm TurboVault image is `develop` | `rebuild-turbovault.sh develop` if unsure | backend healthy; nginx on 3001 |
| **1** | **turbovault** | Life-OS core; vault writes must be capability-gated before anything else | boot log lists tools; `read_note` / search via a life or dispatch session; a Write-denied grant cannot write |
| **2** | **searxng-mcp** | Autonomy needs web search | simple search tool call |
| **3** | **liberado-weather-mcp** | Easy `read_only` canary | forecast call |
| **4** | **caldav-mcp** | Calendar = daily-driver life OS | list events |
| **5** | **mem0-mcp** | Memory; `writes_vault = false` | store + recall |
| **6** | **liberado-pdf-mcp** | Utility | one PDF op |
| **7** | **rentcast-mcp** | Needs key (already in auth stack for OpenClaw) | property lookup |
| **8** | **liberado-calorie-counter-mcp** | Own DB | log meal |
| **9** | **liberado-anythingllm-mcp** | Heavier RAG | one query |
| **10** | **actual / liberado-actual-mcp** | Finance-adjacent â€” rate carefully (`external`?) | read-only first if possible |
| **11** | **spider-mcp** | Heavy (Chrome); last among local | optional |
| **12** | **deepwiki** | Zero local cost | ask about a public repo |
| later | **liberado-wakeup-mcp** | Add Compose service from GitHub *or* managed binary; register + hook | schedule â†’ webhook fire |
| later | **chat-search** / **memory-mcp** | Workspace-local crates â€” ship as containers from this repo or managed on host | â€” |

After **each** step:

```bash
docker compose -f ~/homelab/services/liberado/docker-compose.yml up -d --force-recreate
docker logs liberado --tail 50
curl -fsS http://192.168.0.144:4201/api/status   # chat_tools / dispatcher tools grow
# then a real call: chat "use weather for â€¦" or a goal session that needs the tool
```

If one MCP poisons the shared connect budget, disable it (`enabled = false`) and fix upstream â€”
template topology already documents this failure mode for weather/rentcast.

---

## mcp-sources.toml (keep as the git map â€” not the runtime)

Land (or keep) entries that **mirror** Compose build URLs, for forge/dev and for humans:

```toml
[[source]]
name = "liberado-weather-mcp"
git = "https://github.com/ForrestThump/liberado-weather-mcp"
# rev = "master"

[[source]]
name = "liberado-pdf-mcp"
git = "https://github.com/ForrestThump/liberado-pdf-mcp"
package = "mcp-pdf-server"

[[source]]
name = "liberado-rentcast-mcp"
git = "https://github.com/ForrestThump/liberado-rentcast-mcp"

[[source]]
name = "liberado-anythingllm-mcp"
git = "https://github.com/ForrestThump/liberado-anythingllm-mcp"

[[source]]
name = "liberado-caldav-mcp"
git = "https://github.com/ForrestThump/liberado-caldav-mcp"

[[source]]
name = "liberado-calorie-counter-mcp"
git = "https://github.com/ForrestThump/liberado-calorie-counter-mcp"

# TurboVault: NOT a forge source for the daemon. Document only:
#   rebuild: ~/homelab/scripts/rebuild-turbovault.sh develop
#   runtime: http://turbovault:3001
```

Homelab runtime topology uses `kind = "http"`, not `managed`, for all of the above.

---

## What we deliberately skip / defer

| Idea | Why not (now) |
|---|---|
| Mount Docker socket + `McpTransport::Docker` from liberado | Spawns *new* containers per connect; duplicates Compose services; socket privilege |
| `managed` stdio inside liberado container | No forge binaries in image; HTTP peers already exist; weather historically broken on stdio |
| Rebuilding every MCP via forge on the 4-core box | Compose already builds from GitHub; forge would redo work without process supervision |
| Flipping Liberado vault to `:rw` before TurboVault is gated | F1 lesson â€” capability boundary must be exercised first |
| Wiring all MCPs in one topology edit | Connect failures share budget; one-at-a-time is cheaper than debugging zero tools |

---

## Implementation checklist (when executing)

1. [ ] `rebuild-turbovault.sh develop` if image age / branch is uncertain  
2. [ ] Add turbovault `[[mcps]]` + dispatcher `ExecuteMcp` on host config; recreate; live tool call  
3. [ ] Confirm Write-denied session cannot write via turbovault (F1 live check)  
4. [ ] searxng â†’ weather â†’ caldav â†’ mem0 â†’ â€¦ per table  
5. [ ] Fix `actual-mcp` vs `liberado-actual-mcp` name drift in Compose if still mismatched  
6. [ ] Optional: land `deploy/homelab/config/mcp-sources.toml` as the git map for forge/docs  
7. [ ] Optional later: Compose service for `liberado-wakeup-mcp` from its GitHub repo + hook secret  
8. [ ] Only after MCPs + guards look right: consider vault `:rw` on liberado (probably still
      unnecessary if all writes go through turbovault)

---

## Success criteria

- Liberado `/api/status` shows non-zero tools; dispatcher can `ExecuteMcp` for wired servers  
- Chat/delegate can answer â€œwhatâ€™s on my calendar / weather / search Xâ€ without OpenClaw  
- TurboVault writes respect zones; a Read-only grant cannot mutate the vault  
- No second copy of each MCP process living inside the liberado container  
- GitHub remains the source of truth for Liberado-branded MCP code; TurboVault stays on **`develop`**
