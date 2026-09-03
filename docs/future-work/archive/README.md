# Archive — finished plans, closed audits, resolved findings

Nothing in here describes the system as it is now. This directory keeps only completed work with
reasoning that a current specification, ADR, test, or failure-mode summary does not replace. Routine
implementation history belongs in git, where it does not pollute ordinary repository searches.

They were moved here on 2026-07-14 because 32 roadmap files, 21 of them dead, made the live ones
unfindable. A roadmap you cannot navigate is not a roadmap.

**If you are looking for what is true today, none of this is it.** Start with:

- [`../../roadmap.md`](../../roadmap.md) — what is live, what is next, what is known-broken.
- [`../../spec/architecture/overview.md`](../../spec/architecture/overview.md) — the cold-start map.
- [`../../spec/architecture/failure-modes.md`](../../spec/architecture/failure-modes.md) — **the distilled
  lessons from the audits in this directory.** Read this one. It is the reason it is safe to archive
  the rest: the individual audits found the same handful of bugs over and over, and that pattern —
  not the incident detail — is the part worth carrying forward.

Statuses inside these files were true when written and may be wrong now (`session-focus-plan.md`
still says "no code yet"; S1–S7 all shipped). They are a record, not a claim.

Everything removed from this directory remains in git history.

## Index (partial — not exhaustive)

| File | Kind |
|------|------|
| [session-focus-plan.md](session-focus-plan.md) | Session model build (S1–S7) — shipped |
| [one-execution-engine-plan.md](one-execution-engine-plan.md) | One hub for all goals — shipped |
| [architecture-alignment-audit-2026-07-11.md](architecture-alignment-audit-2026-07-11.md) | Layer rules audit |
| [agentic-mesh-hygiene-audit-2026-07-10.md](agentic-mesh-hygiene-audit-2026-07-10.md) | Mesh framing hygiene |
| [turbovault-vector-prototype-plan.md](turbovault-vector-prototype-plan.md) | Vector prototype — live on homelab |
| [turbovault-vector-module-plan.md](turbovault-vector-module-plan.md) | Vector module notes — superseded by modules umbrella |
| [mcp-homelab-wire-plan.md](mcp-homelab-wire-plan.md) | Homelab MCP wire-up — largely landed |
| [human-todo.md](human-todo.md) | Operator checklist snapshot |
| [webui-flesh-out-plan.md](webui-flesh-out-plan.md) | WebUI flesh-out — all 5 phases implemented; design reference only |
| [mutants-campaign-ledger-plan.md](mutants-campaign-ledger-plan.md) | Mutation campaign ledger CLI and recipes — implemented; live operation is in the skill |
| *Other retained records* | Same directory — treat as historical |

Living roadmap: [`../../roadmap.md`](../../roadmap.md) · Future work index: [`../README.md`](../README.md).

## Moved here 2026-08-08

The retained session-profile record was verified against `main` before moving. The feature is in
the tree, not merely on a branch, which is what its header had drifted on:

| Doc | Verified by |
|-----|-------------|
| `session-profiles-plan.md` | `SessionProfile` resolved in `crates/config-loader/src/model/` — the index still said "parked on `feat/session-profiles`" |
