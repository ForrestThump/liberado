# Context Compaction — Design & Roadmap (CH3)

**Status**: Tier 1 (automatic turn-boundary compaction, persisted markers) shipped 2026-07-23.
**Live-verified** same day against a real daemon (DeepSeek, `trigger_tokens = 1000`,
`keep_recent_turns = 1`): turns 1–2 did not compact; turn 3 fired
(`estimate_before=1792, elided=2, tail=2`), turn 4 fired the **rolling** second compaction
(`estimate_before=1575, elided=3` — the previous marker correctly folded forward); the second
summary carried both seeded facts (code word + wrench); and the model answered the recall question
("what was the code word?") correctly from the summary alone, with the raw region elided from its
context. Follow-ups captured at the bottom, unscheduled.

## Motivation

Chat history grows without bound. Every turn rehydrates the **entire** conversation from the
session store and sends it to the model (`ChatSessions::load` → `Conversation::from_history` →
`Executor::converse_*`), so a long enough conversation eventually exceeds the model's context
window and the turn fails — with no recovery path short of abandoning the session. The roadmap
ask (CH3): *summarize/compress long conversations to stay under model context windows without
losing key facts.*

## Outside research (commissioned 2026-07-23, four projects)

Surveyed how peer open-source agentic-chat systems solve this, via DeepWiki:

| Project | Trigger | What's kept | Summary | Extra mechanism |
|---|---|---|---|---|
| **OpenCode** (`sst/opencode`, `SessionCompaction`) | Pre-turn estimate > context − 20k buffer; one overflow-error retry | Last 2 user turns verbatim (2k–8k token bound) | Rolling structured Markdown template (objective / state / next moves / files); *updates* previous summary | Tool outputs > 2k chars pruned |
| **Kilo Code** (`Kilo-Org/kilocode`) | Configurable `threshold_percent` of window; reserve = min(output cap, 20k–32k) | Last 2 user turns, 25% of usable context clamped 2k–8k | Dedicated (cheaper) compaction model optional; anchored-summarization prompt | **Between-turn prune**: tool results older than a 40k-token recency window → `"[Old tool result content cleared]"`; token estimate × **1.3 safety factor** for code/JSON undercounting |
| **OpenClaw** (`openclaw/openclaw`) | Post-turn `contextTokens > window − reserve`; overflow recovery; preflight byte-size; mid-turn precheck after tool results | Last 3 user turns; **tool calls kept paired with their results** (boundary shifted to avoid orphans) | Compaction entry *in the transcript*; transcript rotates to summary + tail after compaction | `/compact` manual; memory-flush turn before compacting; `toolResultMaxChars` head+tail truncation |
| **LibreChat** (`danny-avila/LibreChat`) | `summaryBaseline` marker — tokens before the marker stop counting | n/a (branch-summing token counter) | Summary marker in history | Meilisearch for history search (validates our Tier-1-first, defer-Tier-2/3 call in [`chat-search-plan.md`](chat-search-plan.md) — a much heavier stack for the same job) |

**Convergent practice across all four:**
1. **Estimate, don't tokenize.** Chars/4 × safety factor (Kilo's 1.3) is what everyone ships; no
   project drags in a real tokenizer for the trigger.
2. **Threshold = context window − reserve**, checked at a turn boundary. Reserve covers tool
   schemas + the reply + estimation slack.
3. **Keep the last K user turns verbatim** (K = 2–3); summarize everything older. Boundary
   anchored on user messages so tool-call/result pairs never split.
4. **Rolling structured summary**, updated not regenerated: goal / constraints / progress /
   key decisions / next steps / relevant files. Only facts present in the transcript.
5. **The marker is part of the transcript** (OpenClaw's compaction entry, LibreChat's
   summaryBaseline), so rehydration resumes from the summary — not from the raw history.
6. **Compaction failure must not fail the turn** — degrade to running uncompacted.

## Design (as shipped)

The one code path every chat host shares is `ChatSessions` — compaction lives there, not in the
executor (which also serves non-chat goal sessions, whose packs own their own context strategies)
and not in any surface.

### The persisted-marker model (the load-bearing choice)

`ChatSessions` rehydrates from the store per turn and holds no in-memory cache — so an
in-memory-only compaction would re-trigger every turn and vanish on restart (the write-only-memory
seam of `failure-modes.md` §5, worn again). Instead, compaction is a **first-class citizen of the
session DAG**, using the existing `Author::Named` identity seam (the same one
`append_note`'s `"goal-session"` author already uses — additive, no schema change):

- Compacting appends a **marker node**: `Author::Named("compaction")`, a system-role message whose
  content is `[context compacted — summary of earlier conversation]` + the rolling summary.
- The **tail** (last K user turns) is then **re-appended verbatim** after the marker (new ids,
  original authors/content). This is what makes the append-only log work: the compacted *view* —
  `[system root, marker, tail…]` — is a contiguous suffix of the log. (OpenClaw does the file-rotation
  equivalent: successor transcript = summary + tail. We keep one log; the DAG absorbs it.)
- `ChatSessions::load` applies the **elision rule**: drop everything strictly between the system
  root and the *latest* compaction marker. Rendered history (`GET /api/conversations/{id}`) is
  unaffected — the full transcript, marker visible as a system bubble (transparency for free).
  `chat-search` still searches the elided content on disk — compaction never deletes.

Raw history stays on disk, auditable and searchable; the model-visible context starts at the
latest marker. Multiple compactions nest naturally: the next summarizer input starts from the
previous marker's summary (rolling update, per the research). One honest cosmetic cost: the
re-appended tail duplicates those messages on disk, so `chat-search` can report the same text
twice *within one conversation* — harmless at this tier, worth a dedup only if it ever annoys.

### Known residual gap (marker-before-tail partial failure)

**Status: accepted residual of Tier 1; closed by construction in the CH3.1 viewport re-architecture
— see [`../plans/context-compaction-viewport-rearchitecture.md`](./plans/context-compaction-viewport-rearchitecture.md).**

Failure modes **before** the marker is durable fail open (run uncompacted): summarizer empty/error,
marker append failure. After the marker is durable, tail re-append is best-effort:

- **This turn** is always complete in memory (every kept tail message is pushed into the model view
  even if a given append fails). Test:
  `partial_tail_reappend_failure_keeps_full_view_for_this_turn`.
- **Next load** applies elision from the latest marker: any tail message that never re-appended is
  **gone from model-visible context**, while the pre-marker originals remain on disk for history /
  search. Operators see `tracing::error` on incomplete tail; there is no automatic repair.

Likelihood is low if `SessionStore` appends are reliable; blast radius is up to the last K user
turns (default 3). Interim mitigations (incomplete-marker non-elision) and the preferred end state
(side summary + `continue_from` viewport, no tail re-append) are spelled out in the CH3.1 plan.

### Trigger

- `estimate_tokens = ceil(chars / 4 × 1.3)` over message contents + tool-call JSON (Kilo's factor;
  no tokenizer dependency).
- Fire when `estimate(history + incoming user message) >` the **resolved** trigger for the face
  model (default path: `trigger_pct` 0.75 × declared `context_window`, else absolute
  `trigger_tokens`, else fallback **48_000** for undeclared models).
- Checked **once per turn, at the turn boundary** (after `load`, before dispatch/execution).
  Mid-turn growth (a tool loop inflating one turn) is the executor's `Budget::TokenLimit`'s job
  today; a mid-turn precheck is a captured follow-up, not v1.

### Summary generation

- One plain `Provider::complete` call on the **chat's own (face) provider** — no new role, no new
  provider wiring (Kilo/OpenCode both default to the session model; a dedicated cheaper summarizer
  is a config follow-up if cost ever shows up).
- Anchored-summarization system prompt + the elided transcript rendered role-labeled
  (`[user]` / `[assistant]` / `[tool result]`), each tool result truncated to
  `tool_result_max_chars` (default 2_000, OpenCode's number).
- Structured Markdown template: **Goal · Constraints & preferences · Progress (done / in progress /
  blocked) · Key facts & decisions · Pending asks & next steps · Relevant files, tools & artifacts**.
  Instructed to use only facts present in the transcript, and to carry a previous summary forward
  when the transcript opens with one.
- Capped at `summary_max_tokens` (default 1_024) so the cure can't become the disease.
- On summarizer error/empty: `tracing::warn`, proceed uncompacted. **Never fail a turn because
  compaction failed** (research point 6; also failure-modes §4 — a machine check may not overrule
  the human's conversation).

### Configuration

`[main_agent.compaction]` in `topology.toml` (all optional; defaults shown). The **trigger** is
resolved per **face model** at chat configure time (no rebuild — edit TOML + restart):

1. Per-model absolute: `[main_agent.compaction.models."<name>"].trigger_tokens`
2. Per-model pct × that model's `[[models]].context_window`
3. Global absolute: `trigger_tokens` when set
4. Global `trigger_pct` × face model's `context_window`
5. Fallback `48_000` when the face model has no declared window

```toml
[main_agent.compaction]
enabled = true              # default ON — a reliability guard that is opt-in is off (failure-modes §2)
trigger_pct = 0.75          # default: 75% of the face model's declared context_window
# trigger_tokens = 48000    # optional global absolute — overrides trigger_pct when set
keep_recent_turns = 3       # user turns kept verbatim (OpenClaw's number; OpenCode/Kilo use 2)
summary_max_tokens = 1024
tool_result_max_chars = 2000

# Per-model overrides (keys match [[models]].name / live provider model slug):
# [main_agent.compaction.models."deepseek-chat"]
# trigger_pct = 0.70
# # trigger_tokens = 40000  # absolute wins over pct for this model

# [[models]]
# name = "deepseek-chat"
# context_window = 64000
# tool_calling = true
# structured_output = false
# tier = "work_plane"
```

`CompactionSettings` lives in `config-loader` (config tier) and owns resolution
(`resolve_trigger_tokens`). The runtime `CompactionConfig` in `liberado-main-agent` still takes a
single absolute `trigger_tokens` (kernel stays model-agnostic); `liberado-server` resolves then
maps in `configure_chat`.

**Note:** trigger resolution runs at chat configure (boot). Process-wide `POST /api/models/select`
does **not** re-resolve the compaction threshold today. True mid-session / per-conversation model
switching (roadmap **CH4** in [`roadmap.md`](roadmap.md)) should re-resolve when the face model
for a conversation changes.

## Deliberately not built (follow-ups, unscheduled)

- **CH3.1 viewport / side-summary re-architecture** — full conversation stays one append-only
  spine; summary is a separate node; session viewport points at summary + `continue_from` for
  model context only. Removes tail re-append and the partial-failure residual above. Plan:
  [`../plans/context-compaction-viewport-rearchitecture.md`](./plans/context-compaction-viewport-rearchitecture.md).
- **Mid-turn precheck** (OpenClaw-style: re-check pressure after each tool result inside the
  executor loop). Needs the executor to own compaction awareness; bigger intrusion, and chat turns
  in face-agent mode rarely loop enough to need it. Revisit if thick-mode chats overflow mid-turn.
- **Between-turn tool-result pruning** (Kilo's 40k-token recency window clearing) — a cheaper,
  summarizer-free first line of defense. Additive later; the marker model doesn't preclude it.
- **Overflow-error-triggered retry compaction** (compact once when the provider rejects for
  context length). The `Provider` error surface has no structured context-overflow signal today —
  it would be stringly matching provider error text. Add a typed variant to
  `provider-openai-compat` first, then this is cheap.
- **Manual `/compact`** slash command + endpoint. The automatic path plus a config-settable
  trigger covers v1; a manual trigger is a thin addition over `ChatSessions` when wanted.
- **Dedicated summarizer model** (`roles` override). One line of wiring once a `ModelRole` for it
  exists; default (face provider) is what OpenCode/Kilo ship by default.
- **Goal-session transcript compaction** (the executor's `Mode::Report` loops). Packs own their
  context; the coding pack's needs differ (file states, diffs). Separate design when needed.

## Files (Tier 1, as built)

- `crates/main-agent/src/compaction.rs` (new) — `CompactionConfig`, token estimate, elision-
  boundary selection, transcript rendering, summarizer prompt, marker identity (`"compaction"`).
- `crates/main-agent/src/sessions.rs` — `ChatSessions::with_compaction`, the elision rule in
  `load`, `maybe_compact` in both `turn` and `turn_stream`.
- `crates/config-loader/src/model/topology.rs` — `MainAgentConfig.compaction: CompactionSettings`.
- `crates/server/src/lib.rs` — `configure_chat` maps config → `with_compaction`.
- `config.example/topology.toml` — commented `[main_agent.compaction]` example.
- Tests: unit (estimate/boundary/transcript) + integration over the **real** `SessionStore`
  (marker persists; next turn's provider request carries the summary, not the elided content;
  under-trigger and summarizer-failure paths) — per `failure-modes.md` §1 doctrine.
