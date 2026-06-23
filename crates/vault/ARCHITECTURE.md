# liberado-vault — the Turbovault adapter + loop-breaking

Liberado's thin adapter over **Turbovault** (the Obsidian-Markdown vault that is the system's source
of truth). It does two things and isolates one risk:

1. **Provenance-tagged writes** — `write` / `move_note` attach a `WriteProvenance` to the write's
   audit-log entry (via Turbovault's `write_*_with_metadata`).
2. **Consumer-side attribution** — `attribute(path)` decides whether an observed change was made by
   one of our agents (suppress) or by something outside our write path (react). This is the
   loop-breaking primitive (Decision 5).
3. It is **the single place** the upstream-dependency fallbacks (concurrency spec §8.1) are isolated.

## How attribution works (`attribution.rs`)

A filesystem change event is provenance-blind — identical whether Turbovault, Obsidian, or git wrote
the file. We attribute by **content identity, not timing**:

1. Hash the current file (`content_hash`, SHA-256 hex).
2. Scan recent audit entries; find the one whose `after_hash` equals the current hash — i.e. the
   write that produced the bytes on disk now. Match against the entry's **resulting** path
   (`new_path` for a Move, else `path`).
3. If that entry's provenance says a non-human agent did it → `Attribution::Agent` (**suppress**).
   No match, or a human/unattributed write → `Attribution::External` (**react**). Unreadable path →
   `Attribution::Missing`.

`should_react(path)` is the boolean convenience over this.

### The invariant that must never break

Loop-breaking suppresses *only* recognized agent writes. A human edit must **always** be reacted to.
Matching on the *resulting* path (not the source) is what prevents a Move from falsely suppressing a
later human re-creation of the moved-from path — the one mistake the system must never make
(regression-tested).

## Dependencies

- Depends on: `liberado-common` (`WriteProvenance`), `turbovault-core`/`-vault`/`-audit`.
- Depended on by: `daemon` (calls `attribute`), and the `provenance_e2e` end-to-end test.

## Tests

`attribution.rs` inline tests cover: agent write suppressed, human-edit-after-agent reacted to,
latest-matching-write wins, **external recreation of a moved source is not suppressed** (the HIGH-
severity regression), human-sourced write not suppressed, missing path. `tests/coverage.rs` covers
the write/error surface. The full cross-component proof is `tests/provenance_e2e.rs`.
