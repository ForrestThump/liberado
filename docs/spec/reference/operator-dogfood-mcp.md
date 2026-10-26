---
kind: reference
status: active
authority: normative
domain: product
canonical_for: operator-dogfood-mcp
open_items: false
last_verified: 2026-10-12
---

# Operator dogfood MCP

`liberado-operator-dogfood-mcp` is a stdio MCP for an external operator agent.
It calls the daemon HTTP API the WebUI already uses. When `[api]` credentials are
configured, set `LIBERADO_API_TOKEN` to a `[[api.clients]]` token so requests
send `Authorization: Bearer …`. Loopback daemons with no `[api]` table still
accept unauthenticated callers.

Build it with `cargo build --release --bin liberado-operator-dogfood-mcp`.
Point the operator agent's stdio MCP config at that binary. Set `LIBERADO_SERVER`
to the origin the WebUI already uses. When unset, the base is loopback port
4201. The client rejects a base URL that carries userinfo, a path, a query, or
a fragment. Redirects are not followed.

The example block in `config.example/topology.toml` stays commented. Do not
grant this MCP to `main-agent`. `send_human_message` posts a human turn, and
the face must not file human replies.

## Tools

| Tool | HTTP | Effect |
|---|---|---|
| `create_agent` | `GET /api/profiles`, `GET /api/conversations`, sometimes `POST /api/conversations` | Find or create one Agents-shelf session |
| `start_chat` | `POST /api/conversations` | Open a normal chat |
| `send_human_message` | `POST /api/chat` | Post one human turn |
| `continue_session` | `POST /api/chat` | Post the next human turn on the same session |
| `read_replies` | `GET /api/conversations/{id}` | Latest assistant message |
| `get_history` | `GET /api/conversations/{id}` | Transcript |
| `list_agents` | `GET /api/conversations` | Agents-shelf rows |
| `list_sessions` | `GET /api/conversations` or `GET /api/sessions` | Slim session rows |
| `status` | `GET /api/status` | Daemon health |
| `catalog` | `GET /api/catalog` | MCP and tool names |
| `workspace_smoke` | `GET /api/status` | Whether the boot catalog lists workspace tools |
| `list_pending_approvals` | `GET /api/approvals/pending` | Open permission requests |
| `get_agent_grants` | `GET /api/conversations/{id}/grant` | Current header grant |

`send_human_message` and `continue_session` are the only tools that post a user
message. Both require the caller to pass `message`.

## Pending approvals

`list_pending_approvals` reads `GET /api/approvals/pending`. Each row has:

- `id` — proposal id
- `conversation` — session that raised the request, or null for a background request
- `tool` — tool name on the blocked call, or null
- `zone` — zone on the requested `Write` grant, or null
- `created_at` — RFC3339 time from the proposal note

The list is pending permission requests only. A decided card stays on
`GET /api/conversations/{id}/approvals`. An ordinary proposal is not a row.

This tool does not decide a request.

## Header grants

`get_agent_grants` reads `GET /api/conversations/{id}/grant`. Pass the session
id from `list_agents`, `list_sessions`, `create_agent`, or `start_chat`. The
id may be a conversation or an Agents-shelf agent. Both are session headers.

The `grant` object is the session header's `SessionGrant` as stored. The route
does not re-resolve `policy.toml` and does not copy in the process-wide default.
An absent `profile` means the header names no hat. The capability list is
`grant.capabilities.capabilities`. A turn with no named profile uses the daemon
default at run time; this route does not substitute that default.

```json
{
  "id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  "surface_mode": "agent",
  "grant": {
    "profile": "coding",
    "delegation": false,
    "capabilities": {
      "capabilities": [
        { "Write": { "Vault": "tasks" } },
        { "ExecuteMcp": "turbovault" }
      ]
    }
  }
}
```

This tool does not change the grant.

## No approve tool

There is no approve tool and no resolve tool. `POST /api/approvals/{id}/resolve`
records a human decision (Deny / Once / "Until restart, all chats" / Everywhere). That route is
not a `GET`, and it is not registered on this MCP. A test scans the tool
declarations and fails if an approve or resolve tool appears. A second test
fails if `list_pending_approvals` or `get_agent_grants` stop using `GET`.

The HTTP contract is [`api.md`](api.md). The approval boundary is
[`contracts.md`](../architecture/contracts.md).
