# Liberado Decision 5 — Vault Concurrency, Write Provenance & Loop-Breaking Spec

**Status**: Resolves Tier-1 Decision 5. Actionable spec; implementation can begin from here.
**Owner**: Shiloh Mangus
**Last Updated**: June 21, 2026
**Related**:
- `liberado-architecture-decisions.md` (Decision 5)
- `life-os-architecture.md` (§5 Triggering Layer, §6 Vault Layer)
- Turbovault `turbovault-write-provenance-proposal.md`
- Turbovault `write-provenance-in-event-stream-issue.md` (Approach A / B analysis)
- Turbovault `turbovault-concurrency-improvements-proposal.md`
- Turbovault `turbovault-audit` crate (`AuditEntry`, append-only JSONL log)

---

## 1. Purpose & Scope

The vault is a shared database with many concurrent writers: the human (in Obsidian), the
main agent, dispatched subagents, and hooks reacting to events. This spec defines the rules
that make those writers coexist safely:

1. **Write zones** — who is allowed to write where.
2. **Provenance** — how every agent-originated write is attributed.
3. **Concurrency** — how simultaneous edits are detected and resolved.
4. **Loop-breaking** — how reactive hooks avoid reacting to their own (and each other's) writes.
5. **Idempotency** — how at-most-once webhook delivery is made safe to retry.

This is load-bearing: no hook or agent that **writes** to the vault should be implemented
before these rules are in place.

---

## 2. Design Decision Summary (the short version)

- **Provenance lives on the Turbovault audit log, not in note frontmatter.** Frontmatter is
  *state* (last-writer-only, and silently wrong the moment a human edits in Obsidian without
  going through Turbovault). The audit entry is a *per-write event* and already carries
  `before_hash` / `after_hash` + a free-form `metadata` field — provenance rides there.
- **Loop-breaking uses Approach A: consumer-side hash join.** A reactive consumer attributes
  an observed change by hashing the file and matching it against the `after_hash` of a recent
  Turbovault write whose provenance says "an agent did this." Match → suppress (it's ours).
  No match → it's an external (human) change → react.
- **We consume Turbovault's *native* change subscription** (PR #24
  `subscribe_vault_events` / `fetch_vault_events`), not a hand-built `vault-change-emitter`.
  The custom emitter described in `life-os-architecture.md` §5 is superseded; see §8.
- **Concurrency stays optimistic**: read returns a hash, writes pass `expected_hash`. We adopt
  the structured `ConcurrentModification { path, expected, actual }` error from the
  concurrency proposal so agents recover programmatically instead of string-parsing.
- **Attribution is best-effort, not a security boundary.** `provenance = None` always means
  "treat as external / unknown," never "trusted." Security is enforced by the capability/zone
  model (Decision 4), not by provenance.

---

## 3. Write Zones & Human-vs-Agent Boundaries

Write authority is expressed in the Decision 4 zone/capability model. Decision 5 adds a
per-zone **write-class** policy that the daemon and hooks honor:

| Write class | Meaning | Examples |
|---|---|---|
| `human_only` | Agents may **read** but never write. Agent writes here are rejected at the boundary. | `journal/`, `people/`, raw `inbox/` |
| `agent_writable` | Agents may write directly (with provenance + `expected_hash`). | `reviews/`, `decisions/` (agent-appended outcomes), `knowledge/` derived notes |
| `proposal_only` | Agents may not mutate directly; they emit a **proposal** to a review location for human approval (ties to Decision 11). | `calendar/` family events, anything `Sensitive` / `FamilyShared` |
| `shared` | Both human and agents write; conflicts handled by optimistic concurrency. | `tasks/`, daily notes |

Rules:
- Write class is declared **per zone** in the same config that holds capability grants
  (Decision 14, single source of truth). Default for an unlisted zone is `proposal_only`
  (fail safe — agents can't silently write somewhere undeclared).
- The class is enforced at the **MCP/hook boundary** (same place capabilities are checked),
  never only in the orchestrator.
- A subagent's write class is the **intersection** of its granted zone class and any
  narrowing applied at dispatch — narrow only, never widen (Decision 4 invariant).

---

## 4. Provenance Model

### 4.1 Data shape

Recorded on the audit entry for every agent-originated write:

```rust
pub struct WriteProvenance {
    /// Who/what performed the write. e.g. "human", "liberado-dispatcher",
    /// "tasks-mcp", "daily-review-agent". Free-form string for v1.
    pub source: String,
    /// Links this write to the task/decision/event that caused it.
    /// REQUIRED for any agent write (used as the loop-breaking + idempotency key root).
    pub correlation_id: Option<String>,
    /// The zone the write targeted (lets consumers filter without re-deriving from path).
    pub zone: Option<String>,
    /// Optional free-form reason.
    pub note: Option<String>,
}
```

### 4.2 Where it is stored

On the Turbovault `AuditEntry.metadata` field (already `serde_json::Value`, no schema change
required) under a reserved key:

```json
{ "_liberado_provenance": { "source": "daily-review-agent",
  "correlation_id": "review-2026-06-21", "zone": "reviews", "note": "..." } }
```

If/when the provenance proposal lands a **typed** field on `AuditEntry`, we migrate to it; the
reserved-key approach works against today's audit log with zero upstream changes.

### 4.3 Required vs optional

- **Human writes** (Obsidian / external): produce audit entries only if they go through
  Turbovault. Direct Obsidian edits produce **no audit entry** — they are detected as
  external by the *absence* of a matching agent write (see §6). This is correct and intended.
- **Agent writes**: `source` + `correlation_id` are **mandatory**. The daemon refuses to issue
  an agent write that lacks a correlation ID, so every reactive write is always traceable and
  loop-breakable.

---

## 5. Concurrency Control

Unchanged foundation (already in Turbovault): atomic temp-file + rename writes, SHA-256 over
NFC-normalized content, optimistic `expected_hash` checked before every mutation, `read_note`
returns the current `hash`.

This spec adopts two refinements from `turbovault-concurrency-improvements-proposal.md`:

1. **Structured conflict error.** `ConcurrentModification { path, expected, actual: Option<String> }`
   surfaced as JSON fields at the MCP layer. On conflict an agent **re-reads, re-evaluates, and
   retries** (bounded retries) rather than blindly overwriting. `actual: None` = file was
   deleted out from under the writer.
2. **`edit_note` TOCTOU close + batch `expected_hash`.** Needed before any multi-file agent
   update path. Until upstream lands these, agents use single-file `write_note` with
   `expected_hash` and avoid `batch_execute` for hash-guarded multi-file writes.

The standard agent write loop is therefore: `read → compute → write(expected_hash) →
on ConcurrentModification: re-read and retry (max N) → on repeated failure: emit a proposal /
escalate to main agent` (never force-overwrite a `human_only`/`shared` zone).

---

## 6. Loop-Breaking (Approach A — consumer-side hash join)

The problem: a hook receives `FileModified("reviews/2026-06-21.md")` and must decide **react or
ignore**. The fs-watcher event is provenance-blind (identical whether Turbovault, Obsidian, or
git wrote it). We attribute by **content identity**, not timing.

### 6.1 Algorithm

```text
on FileModified(path):
    current = read(path)
    h       = sha256(nfc(current))
    entry   = audit.query(path).latest()          # newest audit entry for this path
    if entry && entry.after_hash == h:
        prov = entry.provenance                    # attributable to a Turbovault write
        if prov.source != "human" and is_recent(entry, WINDOW):
            IGNORE   # our own / another agent's write — do not react
        else:
            REACT
    else:
        REACT        # no agent write produced this exact content → external (human) edit
```

Matching on **hash equals `after_hash`** (not "most recent write to this path") is what makes
attribution robust to races, event coalescing, and the human-edits-after-agent case:
- Two agents write the same path, watcher coalesces to one event → whichever `after_hash`
  equals current content wins = correct "last writer authored current state."
- Human edits an agent-authored note in Obsidian → new content, no matching `after_hash` →
  correctly treated as external and reacted to. (This is exactly the false-negative that
  frontmatter provenance would cause and why we reject it.)

### 6.2 Why Approach A first (not server-side enrichment B)

- **Zero upstream changes** to the subscription PR — works the day both land independently.
- Keeps the event stream minimal; no per-event hashing in Turbovault's pump.
- Its semantics are also the correct **fallback** for Approach B's cache-miss case, so building
  A first is never wasted. We add Approach B (`include_provenance` on the subscription) only if
  out-of-process / multi-consumer load makes per-consumer joins painful.

### 6.3 Defense-in-depth: correlation-ID generation guard

Hash-join is the primary mechanism; we add a cheap second guard for the degenerate "agent
write whose content collides with what a human would type" case and for chains:

- Every reactive write carries the **originating `correlation_id`** (not a fresh one) when the
  reaction is a *direct consequence* of the triggering event. A hook maintains a small bounded
  **seen-correlation set**; if an incoming event's joined provenance carries a `correlation_id`
  this hook already acted on, it suppresses. This breaks A→B→A chains across *different* hooks,
  which pure per-path hash-join does not catch.
- A reaction that legitimately starts new work mints a **new** correlation ID (child), and
  records `parent_correlation_id` in its provenance `note`, so chains stay traceable and a
  max-depth guard can stop runaway cascades.

### 6.4 Tunables (single source of truth, Decision 14)

- `WINDOW` — recency window for "this audit entry explains this event" (default 60s; mirrors
  the rename-correlation window the subscription PR already uses).
- `MAX_REACTION_DEPTH` — max correlation chain depth before the daemon halts a cascade and
  emits a proposal instead (default 4).
- `RETRY_MAX` — optimistic-write retries before escalating (default 3).

---

## 7. Event Delivery & Idempotency (bridges Decision 6)

Both the fs subscription (drop-and-resync, best-effort) and bare webhook POSTs are at-most-once.
Hook reaction handlers are therefore **idempotent by construction**:

- The **correlation ID is the idempotency key.** Before acting, a hook checks whether work for
  this `correlation_id` already exists (a pending/working/done marker in the vault under a
  conventional location, e.g. `.liberado/reactions/<correlation_id>.json`, or an in-memory
  bounded set keyed by correlation ID for same-process speed with the vault marker as the
  durable backstop).
- **Vault-as-journal**: a reaction first writes its intent as a *pending* artifact (with
  provenance + correlation ID), then performs work, then marks done. A crash/redelivery
  re-enters at the pending marker instead of double-acting.
- On subscription **drop/overflow**, the documented contract is *resync from authoritative
  state* — the hook re-scans its zone (bounded) rather than trusting it saw every event.

---

## 8. Reconciliation with `life-os-architecture.md` §5 (the emitter)

`life-os-architecture.md` describes a hand-built `vault-change-emitter` that watches paths and
routes to hook webhooks. **This is superseded** by Turbovault's native subscription:

- Turbovault already owns the `notify`-based `VaultWatcher` and (via PR #24) fans it out to
  filtered subscribers with a monotonic `seq` / `since_seq` resume cursor.
- The daemon (Decision 2) holds **one** subscription to Turbovault and does the §6 hash-join +
  §6.3 correlation guard **once, centrally**, then routes high-signal, already-de-looped events
  to the relevant hook. Hooks stay thin: they receive *attributed* events and just run domain
  reaction logic. This removes the per-consumer join cost that Approach A would otherwise incur.
- **Non-vault triggers** (systemd timers, git/docker hooks, homelab sources) still POST the
  standardized event payload to hook webhooks directly — that part of §5 stands. Only the
  vault-watching emitter is replaced.

`life-os-architecture.md` §5 has since been rewritten to reflect this (the "hand-built
`vault-change-emitter`" is marked superseded there, matching the single-subscription,
central-attribution design above). Note that `life-os-architecture.md` as a whole now carries a
superseded-by header pointing at `docs/spec/architecture/overview.md` — treat this concurrency spec, not
that older vision doc, as the source of truth for the emitter design.

### 8.1 Upstream dependency ladder (and fallbacks if it slips)

| Capability | Upstream status | Fallback until it lands |
|---|---|---|
| Native change subscription | PR #24 (not yet rebased/merged) | Daemon runs its own `notify` watcher over the vault path; same hash-join logic. |
| Provenance on audit entry (typed) | proposal (draft) | Use `AuditEntry.metadata._liberado_provenance` reserved key (works today). |
| Structured `ConcurrentModification` | proposal (draft) | Parse existing `ConcurrencyError { reason }` string for hashes (ugly but works); isolate in one adapter fn so the switch is one-line later. |
| `edit_note` TOCTOU close + batch `expected_hash` | proposal (draft) | Single-file `write_note` + `expected_hash`; avoid hash-guarded `batch_execute`. |

The architecture does **not block** on any upstream merge — every row has a working fallback the
daemon can ship with and swap out later behind a thin adapter.

---

## 9. What This Unblocks

With this spec in place, the following become safe to build:
- Any `agent_writable` hook (`reviews-hook`, `decisions-hook`) — they have provenance, idempotency,
  and loop-breaking rules.
- The daemon's central subscription + attribution layer.
- The proposal/approval path (Decision 11) for `proposal_only` zones.

## 10. Open Questions (non-blocking)

1. Reserved metadata key name: `_liberado_provenance` vs a vendor-neutral `_provenance`
   (coordinate with the upstream proposal so we don't collide).
2. Where the durable idempotency markers live: a hidden `.liberado/` vault dir (visible to git,
   easy to inspect) vs out-of-vault daemon state (cleaner vault, but not portable with it).
   Leaning `.liberado/` for auditability.
3. Whether `source` should become an enum (`Human | Agent(name) | Mcp(name)`) once the set of
   writers stabilizes — defer; free-form string is fine for v1.
```
