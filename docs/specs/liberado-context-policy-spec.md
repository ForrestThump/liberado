# Liberado ContextPolicy Spec — A Deliberately Dumb Life Header

**Status**: Specifies the main-agent ContextPolicy (Decision 2 daemon / §1 Main Agent). Actionable.
**Owner**: Shiloh Mangus
**Last Updated**: June 21, 2026
**Related**:
- `life-os-architecture.md` (§1 Main Agent & ContextPolicy)
- `liberado-dispatch-logic-spec.md` (on-demand retrieval via the dispatcher; Detach report delivery)
- `liberado-architecture-decisions.md` (Decision 9 ACP→main messaging; Decision 11 proposals)
- Turbovault read tools (`search`, `read_note`, `search_by_frontmatter`, `query_frontmatter_sql`)

---

## 1. The Core Principle

The main agent is built around a **minimal "system prompt"**: the context that is *always* in
front of the model is tiny — a header **under two short paragraphs** — and everything beyond it is
**pulled on demand**. Intelligence lives in retrieval-when-needed, not in preloading.

ContextPolicy is **deterministic and inference-free**: a bounded vault query + a template. It does
not try to be a smart relevance engine. The smart, on-demand retrieval is the dispatcher's job and
Turbovault's job (§4). Keeping the policy dumb is what keeps it from becoming a tuning nightmare.

Three layers were separated in design discussion; ContextPolicy is only **Layer 1**:
- **Layer 1 — life-context injection (this spec).** Deterministic header, system-owned.
- **Layer 2 — session lifecycle.** User-owned: `/new` to reset, trim-to-window as a dumb backstop,
  optional compaction. Not this spec.
- **Layer 3 — delegated work isolation.** Subagent dispatch. See the dispatch spec.

ContextPolicy is the **safety net that makes Layer 2 user-controllable**: because durable life-state
lives in the vault and is re-injected each session, `/new` is cheap and lossless. You can be
cavalier with the conversation precisely because the header re-hydrates from the vault.

---

## 2. Two Jobs

**Job A — session-start life header** (bounded, deterministic). Assembled once when a session/context
begins (including right after `/new`).

**Job B — per-turn background surfacing** (cheap). Each turn, surface newly-arrived high-signal
items so background autonomy re-enters the main loop: completed **Detached** subagent Reports,
**ACP** outputs, and **pending proposals awaiting approval** (Decisions 9, 11; dispatch spec §10).
This is the inbound channel for everything the system did while you weren't looking.

---

## 3. What the Header Contains (and nothing more)

All fields bounded; all derived by deterministic Turbovault queries — no LLM step.

- **Today + rollup**: date, and a one-line count (tasks due, calendar events, proposals pending).
- **Active goals**: titles + one-line status, capped at `MAX_GOALS`.
- **Recent high-signal decisions**: last few days or `important`-tagged, capped at `MAX_DECISIONS`.
- **Inbox line**: pointers to background results since last seen (Job B).
- **Availability pointer**: which vault zones exist + "ask to load more" — tells the model what it
  *can* pull without dumping any of it.

Concrete shape (illustrative — real output is this small):

```
Today: 2026-06-21 (Sat). 3 tasks due · 1 event · 2 proposals awaiting review.
Goals: [Ship Liberado v1 — in progress] · [Q3 fitness — on track] · [Family trip — blocked].
Recent decisions: daemon-first arch (06-20) · risk-tiered clarify threshold (06-21).
Inbox: detached "decision-review" done → reviews/2026-06-21.md · tasks-acp flagged 1 overdue.
Vault: tasks/ calendar/ decisions/ goals/ reviews/ knowledge/ — ask to load more.
```

**Never in the header**: full tool/MCP schemas, task/calendar/knowledge bodies, raw subagent traces,
entire history. Those are on-demand only.

---

## 4. Pulling More Context (on demand)

When a turn needs more than the header, the main agent expands context two ways:

1. **Direct read via a tiny curated toolset.** The main agent is given a *handful* of read-only
   context tools — Turbovault `search` / `read_note` / `search_by_frontmatter` — and nothing else.
   This is a deliberate, narrow exception to "no schemas in main context": a few read tools, not the
   catalog. It lets the agent self-serve a specific note ("open `reviews/2026-06-21.md`") cheaply.
2. **Via the dispatcher.** Anything that is work, a write, or a richer/ambiguous retrieval goes
   through `liberado-dispatcher`, which fetches/acts with full capability and returns a filtered
   Report. The header's availability pointer is what cues the agent that more exists.

Rule of thumb: **read-a-known-thing → direct tool; figure-something-out or do-something → dispatcher.**

---

## 5. User-Configurable, Minimal by Default

The header is whatever the user wants — its template and caps live in the single config source
(Decision 14). But the **architecture is designed around the minimal default**: a small header is the
intended steady state, not a stripped-down mode. Users can add fields, but the system is correct and
useful with the default header alone.

Layer 2 stays with the user: `/new` resets; trim-to-window is a backstop only (never the primary
strategy — it can orphan a tool call from its result); compaction (summarize-and-continue on the main
thread) is optional and distinct from subagent dispatch.

---

## 6. Tunables (single source of truth — Decision 14)

| Name | Default | Meaning |
|---|---|---|
| `MAX_GOALS` | 5 | Active goals shown in the header. |
| `MAX_DECISIONS` | 5 | Recent high-signal decisions shown. |
| `DECISION_RECENCY` | 7d | Window for "recent" decisions (plus any `important`-tagged). |
| `header_template` | built-in | User-overridable template for the header. |
| `inbox_lookback` | since-last-seen | How far back Job B scans for unsurfaced background results. |

---

## 7. v1 Scope

- Deterministic header assembly (Job A) from Turbovault queries + template.
- Per-turn background surfacing (Job B): detached Reports + ACP outputs + pending proposals.
- The tiny curated read-only toolset for direct expansion (§4.1).
- Everything else on-demand via the dispatcher.

**Deferred**: header personalization UI, smarter inbox prioritization, compaction tuning.

---

## 8. Open Questions (non-blocking)

1. Does Job B's "since last seen" cursor live in daemon state or a `.liberado/` vault marker?
   (Mirror the idempotency-marker decision in the concurrency spec §10 Q2.)
2. Exact membership of the curated read-only toolset — just `search` + `read_note`, or also
   `search_by_frontmatter`? Start with `search` + `read_note`; add by proven need.
