# Skill: Dream — Memory Consolidation for Liberado

A deep reflective consolidation pass over this project's durable knowledge base — adapted to the
Liberado Rust workspace from the generic Anthropic agent "Dream" self-improvement pattern.

**Goal**: synthesize recent session experience into clean, durable, well-organized knowledge so
future agent sessions orient quickly and avoid repeating mistakes. Turn short-term session notes into
long-term expertise *for this specific codebase*.

---

## Project context

**Liberado** (aka "life-os") is a Rust-native personal AI **Life OS**: a daemon that watches an
Obsidian vault, reasons about changes with an LLM, and acts via tools — safely, and without reacting
to its own writes. It is a Cargo workspace (edition 2024, rust 1.90), co-developed with sibling repos
`turbovault` and `turbomcp` (consumed as path deps, excluded from this workspace). **Daemon-first**:
one `liberado` binary hosts the watch loop + chat + an HTTP/SSE API; every interface (web UI, the
`liberado chat` CLI client, a future TUI) is a *client* of that API. Durable knowledge lives in
the `docs/` wiki (see `docs/README.md`).

## The memory files (READ them yourself with your tools — they are NOT inlined here)

**Primary orientation set** — consolidate and de-stale these (all live under `docs/`):
- `docs/architecture/overview.md` — cold-start system map, crate table, current status.
- `docs/specs/liberado-architecture-decisions.md` — the **numbered Decision log** (load-bearing and
  authoritative). Treat resolved Decisions as history: you may de-stale a reference, fix a broken
  link, or add a dated clarification, but do **not** rewrite a Decision's meaning.
- `docs/impl/AGENTS.md` — build / run / extend guide (the `liberado` binary, env vars, endpoints).
- `docs/roadmap/current.md` — forward work + nice-to-haves.
- `docs/reference/api.md` — the chat HTTP/SSE contract + interface roadmap.
- `docs/project/handoff.md` — the current-state handoff (historical snapshot: `docs/future-work/ideas/archive/handoff.md`).

**Secondary** — scan for staleness, but do **not** wholesale-rewrite (these are stable design specs):
- `docs/specs/liberado-*-spec.md` (conversation-store, dispatch-logic, context-policy, config,
  vault-concurrency, testing-and-eval, inbox, maintenance-and-git, permissions).
- `docs/specs/life-os-architecture.md`, `docs/specs/liberado-config-spec.md`, etc.
- `crates/*/ARCHITECTURE.md` (per-crate maps).

## Gather your own recent signal

You have repo tools. Reconstruct what recently changed from:
- `handoff.md`.
- **`git status` and `git diff`** — this project carries substantial *uncommitted* work; the diff is
  the richest record of the current session's changes.
- `git log --oneline -20`.
- The **RECENT SESSION SIGNAL** the caller appends at the bottom of this prompt (if present).

Cross-check every claim against the actual code and docs. Prefer verified repo reality over any
narrative.

---

## Phases

**Phase 1 — Orient.** Read each memory file. Note its purpose, and identify duplicates, overlap, and
stale or contradicted sections (especially anything the current code/diff disproves).

**Phase 2 — Gather & Analyze.** Extract high-value signal from the diff/log + session signal. Note
recurring patterns, successful heuristics, failure modes, architecture insights, house style/voice
preferences, and tooling/workflow lessons. Flag contradictions with existing memory or codebase
reality.

**Phase 3 — Consolidate & Improve.** Merge duplicates. Resolve contradictions (prefer fresher,
verified, more-successful information). Convert relative dates to absolute. Prune one-off noise.
Surface **cross-session insights** that only emerge over multiple sessions (e.g. "this approach
consistently causes X in our setup — prefer Y"). Keep everything concise, actionable, structured.

**Phase 4 — Output.**
1. **Apply** the consolidations directly to the memory files (you have Edit/Write), **conservatively**:
   preserve each file's existing structure and the house voice (concise; explains the *why*; no
   emojis). Don't invent. Don't rewrite stable specs. Don't alter resolved Decisions' meaning.
2. **Fix `handoff.md`** into a real, current-state handoff: what's done, what's next, where key
   things live, and any live constraints.
3. **Write a dated report** to `Dreams/<YYYY-MM-DD>-dream.md` containing: (a) what you
   consolidated / updated / pruned / added, file by file; (b) the **cross-session insights** you
   distilled; (c) anything you flagged but deliberately did **not** change (for human decision); and
   (d) contradictions found and how you resolved them.
4. End your reply to the caller with a short summary: *what was consolidated, updated, pruned, or
   newly discovered.*

## Steering

Focus especially on: **Rust idioms + the house style**; **architecture decisions** (daemon-first; the
executor's "termination follows the consumer" seam; Decision 17's append-only node-log storage); the
**established working patterns**; and **common pitfalls in this repo**. Ignore transient debugging
notes unless they reveal a broader lesson. Prioritize verified outcomes over hypotheses.

## Hard constraints

- **Never print or echo `DEEPSEEK_API_KEY` or any secret** (if you must confirm it, report only
  length/prefix).
- **Do not commit to git**, push, or run servers/daemons.
- **Preserve the house voice**; no emojis in code or docs.
- Be **additive/de-staling** with the Decision log and specs — never destructive.
- This is a memory pass, not a feature pass: don't change code behavior.

---

## RECENT SESSION SIGNAL

*(The caller appends the latest session arc below. If empty, rely on `git diff`/`git log` +
`handoff.md`.)*
