---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0014
open_items: false
---

# ADR-0014: Single Source of Truth for Config / Topology

| Field | Value |
|-------|-------|
| Status | accepted |
| Date | 2026-07-02 (from consolidated decision log; see git history) |
| ID | ADR-0014 |

## Context

See **Full historical body** for the original framing, open questions, and design discussion.

## Decision

Single source of truth is one resolved, validated config model assembled from topology.toml, policy.toml, and optional tuning.toml (defaults in code; files hold deltas). Secrets are references, not config values. Agents never write config. Fail-fast cross-validation before serving.

## Consequences

Small or absent config files still boot. Conflicts surface at load time via `liberado config check` and daemon startup, never as runtime surprises.

## Rejected alternatives

One giant hand-edited file as the only representation. Config-as-vault-notes written by agents. Runtime soft-fail on invalid topology.

## Implementation and tests

- `liberado-config-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated decision log)
- **Superseded by:** (none)

## Full historical body

The text below is preserved from `docs/spec/architecture-decisions.md` so reasoning is not lost.

---

Ports, socket paths, webhook URLs, subscription routing, capability grants, and all per-spec tunables.

**Status**: Complete (specified in `liberado-config-spec.md`).

Decision 14: **Single source of truth = one resolved, validated *model*, not one file.** Many small files (split by concern) are merged into one typed config object at startup. Key points:
- **Three concerns, owned distinctly**: `topology.toml` (wiring — components/ports/sockets/models), `policy.toml` (the central, auditable **security surface** — zones, write-classes, capability grants, secret references), and an optional `tuning.toml` (benign behavior overrides). Each setting is owned by exactly one place (validator rejects duplicate ownership).
- **Defaults live in code; config holds only deltas** — every tunable has a `Default` matching its home spec, so the config file can be **small or absent** and the system still works.
- **Out of the vault, homelab-local** (ssh in to edit); **agents never write config** (user-approval-gated config-through-the-system is a v2+ item). **Secrets are not config** (env/systemd by reference — Decision 10).
- **Fail-fast**: merge precedence is defaults ? files ? env (`LIBERADO_*`) ? CLI; the merged whole is **cross-validated before the daemon serves anything** (unknown zones, missing MCPs, port collisions, dangling secret refs, triggerless hooks, etc.), surfaced on startup and via a `liberado config check` command. Conflicts are a load-time error, never a runtime surprise.
