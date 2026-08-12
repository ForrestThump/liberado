---
kind: decision
status: accepted
authority: normative
domain: architecture
canonical_for: adr-0014
open_items: false
---

# ADR-0014: Single Source of Truth for Config / Topology

**Status:** accepted  
**Date:** 2026-07-02 (last update of the consolidated decision log; see git history for earlier revisions)  
**ID:** ADR-0014 (`single-source-config`)

## Context

Recorded as Decision 14 in the historical architecture decision log.

## Decision

**Single source of truth = one resolved, validated *model*, not one file.** Many small files (split by concern) are merged into one typed config object at startup. Key points:
- **Three concerns, owned distinctly**: `topology.toml` (wiring — components/ports/sockets/models), `policy.toml` (the central, auditable **security surface** — zones, write-classes, capability grants, secret references), and an optional `tuning.toml` (benign behavior overrides). Each setting is owned by exactly one place (validator rejects duplicate ownership).
- **Defaults live in code; config holds only deltas** — every tunable has a `Default` matching its home spec, so the config file can be **small or absent** and the system still works.
- **Out of the vault, homelab-local** (ssh in to edit); **agents never write config** (user-approval-gated config-through-the-system is a v2+ item). **Secrets are not config** (env/systemd by reference — Decision 10).
- **Fail-fast**: merge precedence is defaults ? files ? env (`LIBERADO_*`) ? CLI; the merged whole is **cross-validated before the daemon serves anything** (unknown zones, missing MCPs, port collisions, dangling secret refs, triggerless hooks, etc.), surfac…

## Consequences

See the full decision body below for implications, trade-offs, and interactions with other ADRs.

## Rejected alternatives

Where the original log listed open options and a recommended path, the recommended path is the accepted decision. Alternatives discussed in the body were not adopted as the primary design.

## Implementation and tests

- `liberado-config-spec.md`

## Supersedes / superseded by

- **Supersedes:** (none — original decision number from the consolidated log)
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
