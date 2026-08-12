---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0010
open_items: false
---

# ADR-0010: Secrets Backend and Inter-Component Auth

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0010 (`secrets-and-inter-component-auth`)

## Context

Critical for any MCP or hook that touches credentials (email, finance, notifications).

## Decision

Layered, leveraging what turbomcp already provides (API-key/JWT auth, secret zeroization, SSRF/path-traversal guards in `turbomcp-server`/`turbomcp-proxy`):
- **Secrets at rest**: environment variables + **systemd credentials** (`LoadCredential=`) for v1. Each MCP/hook process receives only the secrets it needs, injected at the process boundary.
- **Secret isolation (IronClaw pattern, per `liberado-permissions-idea.md`)**: raw secrets **never enter LLM context** — they are injected at the MCP boundary for the specific authorized operation only. The model sees results, not credentials.
- **Provider/inference keys live only in the daemon.** The main agent, dispatcher, and subagents run inference through the daemon's provider abstraction. MCPs/hooks that need reasoning use **MCP sampling** (`turbomcp-client`) so they never hold provider keys.
- **Inter-component auth**: local MCPs are reached over **Unix domain sockets** (filesystem permissions are the boundary; no network, no token needed). **Hook webhooks** (which accept input from external triggers) require a **shared-secret bearer header** and bind **Tailscale/localhost only**. Start with API-key/shared-secret; **JWT or mTLS** is…

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-permissions-idea.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
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
