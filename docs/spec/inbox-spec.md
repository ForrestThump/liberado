# Liberado Capture & Ambient Analysis Spec — Inbox + Whole-Vault Awareness

> [!WARNING]
> **NOT IMPLEMENTED as of 2026-08-08.** This spec describes the intended design, not the running
> system. `[tuning.capture]` parses and validates but **no code reads it**: there are no settle
> windows, no `#ready-now` / `#hold-off` handling, no ambient sweep, and no watcher ignore list.
>
> What *is* live: the vault watcher fires on changes under the vault, and a change gets the generic
> "a note changed, decide how to react" reaction. The inbox layer above that — tiers, quiescence,
> flags — is the part this spec still only specifies.
>
> Found by dogfooding on 2026-08-08, after config that reads as live sent someone debugging a
> capture pipeline that had never been built.

**Status**: Specifies the async-capture interaction mode and the lighter ambient analysis of all
human notes. A peer to the TUI. Actionable.
**Owner**: Shiloh Mangus
**Last Updated**: June 21, 2026
**Related**:
- `life-os-architecture.md` (interaction modes; hooks; triggering)
- `liberado-dispatch-logic-spec.md` (everything routes through normal dispatch)
- `liberado-vault-concurrency-spec.md` (loop-breaking, write classes, journal markers)
- `liberado-context-policy-spec.md` (counts in header; results via Job B)
- `liberado-vault-maintenance-and-git-spec.md` (Syncthing-conflict merge; git backstop)
- `liberado-architecture-decisions.md` (Decision 6 idempotency, Decision 11 proposals)

---

## 1. The Idea

A **second first-class interaction mode** alongside the TUI: write notes in Obsidian on any device;
Syncthing replicates them to the homelab; the system analyzes them and acts **proportionally to how
actionable they are** — from "this is an explicit to-do, go research and flesh it out" down to
"a note appeared somewhere, just notice it." **No running conversation required.** Capture in the
moment, let the homelab organize, review later.

It is deliberately **patient, not reactive**: if you're mid-thought, nothing should act until you've
clearly stopped. Capture has no urgency by default.

## 2. Why It's Almost Free

It rides existing machinery; the **inbox-hook is thin** because all judgment (what is this note,
where does it go, is it high-consequence) is the dispatcher's existing job (dispatch spec §5–§6):

```
note written/edited  →  Syncthing  →  Turbovault watcher  →  daemon subscription
   →  attributed external/human (no matching agent write) → candidate for analysis
   →  intent tier resolved (flags + location) → settle/quiescence reached
   →  dispatch "analyze this note at intensity T" → dispatcher routes (Execute | Subagent | Propose)
   →  outputs to the right zones; for inbox items, original moved to processed/ with a breadcrumb
```

## 3. Intent Tiers — actionability scales with signal

Two override **flags** (work in *any* note, anywhere) plus **location** as the default signal:

| Tier | Trigger | Intensity | Timing |
|---|---|---|---|
| **Suppressed** | `#hold-off` in the note | nothing until the flag is removed | — |
| **Act-now** | `#ready-now` in the note (anywhere) | full | short settle (`READY_NOW_SETTLE`, ~2 min) |
| **Actionable** | note in `inbox/`, no flag | full — create tasks, research that benefits the user, flesh out ideas, propose | long settle (`INBOX_SETTLE_WINDOW`, ~15 min) |
| **Ambient** | any other human note created/edited, no flag | light — index, suggest links, note for context; **no proactive research or actions** | batched scheduled sweep, not per-edit |

- **Flags override location.** `#ready-now` in a buried note promotes it; `#hold-off` in an inbox note
  parks it.
- **Intensity is an input to the dispatcher**, biasing the action: Actionable/Act-now tiers may spawn
  research subagents and create tasks; Ambient is capped at cheap indexing / link suggestions / at
  most a gentle proposal. This keeps tokens and noise down.

## 4. Reactive vs Swept (the cost split)

- **Actionable + Act-now → reactive**, gated by the settle/quiescence window (§5). These are
  high-signal and few, so per-note inference is justified.
- **Ambient → a low-priority scheduled sweep** (timer-triggered, e.g. nightly) over notes changed
  since the last sweep — *not* a reaction to every edit. A synced vault you edit all day would make
  per-edit ambient analysis a token firehose; most edits warrant no action, so a cheap batched
  "anything worth surfacing here? usually no" pass is the right shape.

## 5. Settle / Quiescence (don't act while the human is typing)

Process a note only after it has been **unmodified for the tier's settle window**, resetting the
timer on every modification event, ideally confirmed by **two equal content hashes** spaced apart.
This is distinct from the loop-breaking window in the concurrency spec ("did an agent write this") —
here it is "has the human stopped editing." They compose. `#hold-off` short-circuits to "never."

## 6. Zero-Friction Capture (the core principle)

- **Default: no structure.** Any text is valid; the dispatcher infers intent (task / thought /
  decision / question). Dumping a sentence must stay frictionless.
- **Optional hints** for determinism: the flags above, or `inbox/<subfolder>/` conventions. Never
  required.

## 7. Syncthing Handling

1. **Ignore Syncthing/editor artifacts**: never analyze `*.sync-conflict-*`, `.stversions/`,
   `*.tmp`, `~*`, hidden files.
2. **Sync conflicts are not fatal** — they are handled out-of-band by the maintenance task
   (`liberado-vault-maintenance-and-git-spec.md`): a subagent does a **lossless union merge** of the
   conflicting versions, deletes the `.sync-conflict` file, and commits (git is the undo). The
   inbox-hook itself just skips conflict files.
3. **Minimize concurrent edits**: read → produce output elsewhere → for inbox items, move the
   original to `processed/` once (the move is the smallest conflict surface and the "done" marker).

## 8. Idempotency & Loop-Breaking

- Each processed note gets a `correlation_id` on first pickup (reaction journal — Decision 6);
  redelivery finds the marker and does not reprocess.
- For inbox items, **departure from `inbox/`** is the terminal state. For ambient sweeps, a
  per-note last-analyzed cursor (hash or mtime) prevents re-analyzing unchanged notes.
- Agent outputs are agent-sourced writes (provenance + correlation_id), de-looped normally — filing
  a thought into `knowledge/` does not re-trigger analysis as if it were new human input.

## 9. Conservatism (Decision 11 + git, see maintenance spec)

- **Low-risk, in-vault routing executes directly** (create a task, file a thought into an
  `agent_writable` zone) — and git makes these recoverable, so we can be liberal here.
- **External or real-world-consequential** actions (scheduling family events, sending, anything
  `Sensitive`/`FamilyShared`) still emit a **proposal**. Git protects vault *data*, not real-world
  *side effects* — so the proposal boundary stays exactly there.

## 10. Closing the Loop on the Capture Surface

- Inbox items: original moved to `processed/` with an **appended breadcrumb** ("→ created
  `tasks/...`; filed thought in `knowledge/...`"), which syncs back to the phone — outcome visible
  **in Obsidian, where it was captured**.
- Ambient results are quiet by design (links/index); anything worth your attention surfaces via
  ContextPolicy Job B. Pending inbox count rides in the header rollup.

## 11. Tunables (single source of truth — Decision 14)

| Name | Default | Meaning |
|---|---|---|
| `inbox_path` | `inbox/` | Actionable-by-default capture folder. |
| `processed_path` | `processed/` | Where handled inbox notes are moved (with breadcrumb). |
| `INBOX_SETTLE_WINDOW` | 15 min | Quiescence before an actionable note is processed. |
| `READY_NOW_SETTLE` | 2 min | Shorter quiescence for `#ready-now` notes. |
| `ready_flag` / `hold_flag` | `#ready-now` / `#hold-off` | Override flags (any note). |
| `ambient_sweep_schedule` | nightly | When the low-intensity whole-vault sweep runs. |
| `ambient_intensity_cap` | index+suggest | Ceiling on what ambient analysis may do (no proactive actions). |
| `inbox_ignore_globs` | `*.sync-conflict-*`, `.stversions/`, `*.tmp`, `~*` | Never-process patterns. |

## 12. v1 Scope

- `inbox-hook`: watch `inbox/` via the daemon subscription, resolve tier (flags + location), settle,
  dispatch at the tier's intensity, move inbox items to `processed/` with a breadcrumb.
- Ambient nightly sweep over changed notes at capped intensity.
- Flags `#ready-now` / `#hold-off`.
- Idempotency via journal + note departure / last-analyzed cursor.

**Deferred**: batching bursts of related captures, inbox digests, smarter ambient prioritization.

## 13. Open Questions (non-blocking)

1. Ambient sweep cadence — nightly vs a few times a day? Start nightly; cheap to change.
2. Should `#ready-now` survive into `processed/` history or be stripped on completion? Lean strip.
