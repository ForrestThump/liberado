# liberado-session — goal session kernel

**Role:** domain-neutral goal session types + in-process hub. Surfaces (TUI/WebUI/CLI) are
**clients**; domain packs implement [`DomainPackRunner`].

## Types

| Type | Role |
|---|---|
| `GoalSpec` | Start a goal (description, criteria, domain, payload) |
| `SessionEvent` / `SessionEventKind` | SSE/TUI event envelope — since 2026-07-11 the **one** event vocabulary for goal sessions *and* chat turns (chat's in-process `AgentEvent` maps onto it at the server boundary; wire mirror in `chat-client-contract`). Includes `Token` for streamed deltas; hard errors are `Failed` (tag `failed` — browser `EventSource` reserves `error`) |
| `GoalSessionStore` | In-memory records + broadcast bus |
| `GoalSessionHub` | Register packs, start/cancel, fan-out events |
| `LifeOpsDemoRunner` | Second-domain proof (no coder-tools) |
| `CodingSessionPack` | In `liberado-coder-agent` — bridges Liberado loop |

## HTTP (server)

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/goals/domains` | Registered packs |
| GET | `/api/goals` | List sessions |
| POST | `/api/goals` | Start session (`GoalSpec` JSON) |
| GET | `/api/goals/{id}` | Snapshot + history |
| GET | `/api/goals/{id}/stream` | SSE (history then live) |
| POST | `/api/goals/{id}/cancel` | Cooperative cancel |

## Dogfood example

```http
POST /api/goals
{"description":"file vault note and mark task done","domain":"life","success_criteria":["note written","task done"]}

GET /api/goals/{id}/stream
```

Coding (when provider configured):

```http
POST /api/goals
{"description":"create hello.txt with hello","domain":"coding","payload":{"workspace_root":"C:/tmp/ws"}}
```
