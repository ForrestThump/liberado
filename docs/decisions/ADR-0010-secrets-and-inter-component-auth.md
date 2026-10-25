---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0010
open_items: false
---

# ADR-0010: Secrets Backend and Inter-Component Auth

| Field | Value |
|-------|-------|
| Status | not implemented |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0010 |

The metadata `status` stays `accepted` because the decision vocabulary has no `not implemented` value. `accepted` means the design was selected. The status table is the implementation mark.

The bind and component-auth mechanisms in this decision are not in the code. `crates/server/src/lib.rs` binds `0.0.0.0` and applies `CorsLayer::permissive()`. The HTTP API has no caller authentication. Local MCPs use stdio or streamable HTTP (`McpTransport` in `crates/config-loader`), not Unix sockets. Two pieces of this decision do exist: hook webhooks check a shared secret (`crates/server/src/hooks.rs`), and `secret_refs` must be set in the environment (`crates/config-loader`).

## Context

See **Full historical body** for the original framing, open questions, and design discussion. The historical body still says "Status: Complete". That sentence is the July 2026 log.

## Decision

Layered secrets and auth: env + systemd credentials for secrets at rest; raw secrets never enter LLM context (injected only at MCP boundary); provider keys only in the daemon; local MCPs over Unix sockets with filesystem permissions; hook webhooks require shared-secret bearer and bind Tailscale/localhost only. JWT/mTLS documented as upgrade for network exposure.

## Consequences

Aligns with turbomcp auth/SSRF guards. MCPs/hooks that need reasoning use sampling through the daemon rather than holding provider keys.

## Rejected alternatives

Passing raw secrets through liberado or main-agent context. Network-exposed hooks without shared-secret auth.

## Implementation and tests

- `liberado-permissions-idea.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

**Why it matters**: Critical for any MCP or hook that touches credentials (email, finance, notifications).

**Recommended path**:
- Use environment variables + systemd credentials for secrets in v1.
- Webhook auth: Shared secret header + Tailscale-only listening (or mTLS later).
- Never pass raw secrets through liberado or main agent context.

**Status**: Complete

Decision 10: Layered, leveraging what turbomcp already provides (API-key/JWT auth, secret zeroization, SSRF/path-traversal guards in `turbomcp-server`/`turbomcp-proxy`):
- **Secrets at rest**: environment variables + **systemd credentials** (`LoadCredential=`) for v1. Each MCP/hook process receives only the secrets it needs, injected at the process boundary.
- **Secret isolation (IronClaw pattern, per `liberado-permissions-idea.md`)**: raw secrets **never enter LLM context** — they are injected at the MCP boundary for the specific authorized operation only. The model sees results, not credentials.
- **Provider/inference keys live only in the daemon.** The main agent, dispatcher, and subagents run inference through the daemon's provider abstraction. MCPs/hooks that need reasoning use **MCP sampling** (`turbomcp-client`) so they never hold provider keys.
- **Inter-component auth**: local MCPs are reached over **Unix domain sockets** (filesystem permissions are the boundary; no network, no token needed). **Hook webhooks** (which accept input from external triggers) require a **shared-secret bearer header** and bind **Tailscale/localhost only**. Start with API-key/shared-secret; **JWT or mTLS** is the documented upgrade for any component that ever becomes network-exposed.
