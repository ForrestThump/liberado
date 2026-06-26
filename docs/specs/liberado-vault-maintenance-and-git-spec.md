# Liberado Vault Maintenance & Git Backstop Spec

**Status**: Specifies the git-backed vault operational model and the maintenance task category.
Actionable.
**Owner**: Shiloh Mangus
**Last Updated**: June 21, 2026
**Related**:
- `liberado-inbox-spec.md` (defers Syncthing-conflict resolution here)
- `liberado-vault-concurrency-spec.md` (provenance, audit log, write classes)
- `liberado-architecture-decisions.md` (Decision 11 proposals; Decision 12 audit trails)
- Turbovault health tools (`get_broken_links`, `quick_health_check`, `full_health_analysis`,
  `get_dead_end_notes`, `get_isolated_clusters`)

---

## 1. Two Ideas, One Spec

1. **Git-backed vault** as a recovery backstop that lowers the stakes of agent writes.
2. **Maintenance tasks** — periodic/on-demand vault hygiene (conflict resolution, cleanup, link
   repair) that the backstop makes safe to run with real autonomy.

## 2. Git-Backed Vault

The vault is a git repository on the homelab. Every change is recoverable, which (a) provides a
coarse recovery trail complementary to the Turbovault audit log and runtime trace (Decision 12),
and (b) **changes the autonomy calculus**: in-vault edits, merges, and cleanup can be done directly
because a mistake is a `git revert` away.

### 2.1 The Syncthing + git reconciliation (load-bearing footgun)

**Never let Syncthing sync the `.git/` directory.** Replicating git internals across devices
corrupts the repo (concurrent packfile/index writes from multiple nodes). The model:

- **The homelab is the git authority.** It is the only node that runs git operations and holds
  `.git/`.
- **Syncthing replicates only the working tree** (the markdown). Devices (phone, laptop) are dumb
  file editors with no `.git/`.
- Enforce with a Syncthing **`.stignore`** entry for `.git/` (and `.turbovault/`, `.liberado/` —
  machine-managed dirs that should also stay homelab-local).

### 2.2 Commit cadence

- Commit on the homelab on a **batch/scheduled basis** (e.g. every agent-write batch, and/or a
  periodic timer), not per-keystroke. Commit messages record provenance (which agent/correlation_id)
  so history is greppable.
- Human edits arriving via Syncthing are committed by the same homelab cadence (attributed
  "external/human"). The git log thus interleaves human and agent changes with attribution.

### 2.3 What git protects — and what it does not

- **Protects**: vault *data* — any note edit, move, delete, or merge is reversible.
- **Does NOT protect**: real-world *side effects* — a sent message, a created calendar invite, an
  external API call cannot be reverted by git.

This line is exactly the proposal boundary (Decision 11): **in-vault operations may act directly
(git is the undo); externally-consequential operations still require a proposal.** Git relaxes
conservatism for vault edits without touching the human-in-the-loop gate for real-world actions.

## 3. Maintenance Tasks

A category of vault-hygiene work run by a scheduled `maintenance-acp` (timer-triggered) and/or
dispatched on demand by the user ("clean up the vault"). Each maintenance run commits its work
(git = undo) and respects write classes.

### 3.1 Syncthing conflict resolution (lossless union merge)

- Find `*.sync-conflict-*` files (Turbovault search / fs scan).
- For each, dispatch a **subagent** that reads the base note + the conflict version and produces a
  **lossless union merge**: preserve every piece of unique substance from both sides; never silently
  drop content. Verification step in the subagent's success criteria: "no unique line/idea from
  either version is absent from the merge."
- Write the merged note, **delete the `.sync-conflict` file**, commit. If the subagent is unsure it
  preserved everything, it emits a **proposal** instead of acting — but because git backstops it,
  confident merges can proceed directly.

### 3.2 Vault hygiene & cleanup

- **Broken links**: Turbovault `get_broken_links` → repair or propose fixes.
- **Orphans / dead-ends / isolated clusters**: `get_dead_end_notes`, `get_isolated_clusters` →
  suggest links or surface for review (ambient-intensity; don't aggressively restructure).
- **Irrelevant / stale content pruning**: conservative — **propose** deletions of clearly-stale
  machine-generated cruft, or rely on the user's manual pass; git makes accidental deletion
  recoverable, but pruning human-authored content always proposes first.
- **Health**: `quick_health_check` routinely; `full_health_analysis` occasionally (it's the
  expensive 1–5s call — use sparingly).

### 3.3 On-demand maintenance dispatch

The user can trigger any of the above ad hoc ("resolve conflicts", "fix broken links", "tidy
knowledge/"). This routes through the normal dispatcher; maintenance is just a goal class with good
Turbovault tooling.

## 4. Idempotency & Loop-Breaking

- Maintenance writes are agent-sourced (provenance + correlation_id) and de-looped normally — a
  link-repair commit does not re-trigger analysis as new human input.
- Conflict-merge runs are keyed by the conflict file's identity; a resolved (deleted) conflict is
  terminal.

## 5. Tunables (single source of truth — Decision 14)

| Name | Default | Meaning |
|---|---|---|
| `git_commit_schedule` | per-batch + hourly | When the homelab commits. |
| `git_authority_node` | homelab | The only node running git ops / holding `.git/`. |
| `stignore_machine_dirs` | `.git/`, `.turbovault/`, `.liberado/` | Dirs Syncthing must not replicate. |
| `maintenance_schedule` | weekly | When `maintenance-acp` runs the hygiene sweep. |
| `prune_requires_proposal` | true (human-authored) | Pruning human content always proposes first. |
| `conflict_merge_autonomy` | direct-if-confident | Confident union merges act directly (git backstop); else propose. |

## 6. v1 Scope

- Git-backed vault on the homelab with `.stignore` excluding machine dirs; batched commits with
  provenance in messages.
- `maintenance-acp`: scheduled conflict resolution + broken-link repair + health check.
- On-demand maintenance dispatch.

**Deferred**: automatic stale-content pruning beyond proposals, richer git-based time-travel UI,
cross-device git (stays homelab-authoritative).

## 7. Open Questions (non-blocking)

1. Commit granularity — one commit per agent action (fine-grained revert) vs batched (cleaner log)?
   Lean batched with correlation_ids in the message; revisit if fine-grained revert is needed.
2. Should the homelab auto-`git gc` / prune history, or keep full history indefinitely? Lean keep;
   markdown is tiny.
3. Does maintenance ever run while the user is actively editing (Syncthing mid-sync)? Gate it behind
   the same settle/quiescence idea as capture to avoid merging a file that's still arriving.
