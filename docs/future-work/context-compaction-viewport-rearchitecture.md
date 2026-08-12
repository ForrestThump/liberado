---
kind: plan
status: active
authority: implementation
domain: chat
canonical_for: context-compaction-viewport
open_items: true
---

# Context compaction re-architecture — viewport / side-summary model (CH3.1)

**Status**: Proposed — not shipped.  
**Depends on**: CH3 Tier 1 (persisted marker + tail re-append), shipped 2026-07-23 — see [`context-compaction-plan.md`](context-compaction-plan.md).  
**Motivation**: Close a known residual failure mode in the marker-before-tail design, remove tail duplication, and make “model context” an explicit viewport over an unbroken conversation log.

---

## 1. Problem with the shipped model (CH3)

### What shipped

CH3 treats compaction as a **rewrite of the leaf path**:

1. Summarize the elided region (everything older than the last K user turns).
2. Append a **marker** node (`Author::Named("compaction")`) off the current leaf.
3. **Re-append** the kept tail as **new** nodes after the marker (same content, fresh ids).
4. On load, **elide** everything strictly between the system root and the latest marker.

The model-visible log becomes a contiguous suffix: `[system root] → [marker] → [tail copies] → [new turns…]`. Pre-marker originals stay on disk for history UI and chat-search.

### Known residual gap: marker-before-tail partial failure

Failure modes **before** the marker is durable are safe (fail-open → run the turn uncompacted):

- summarizer empty / error  
- marker append fails  

The awkward case is **after** the marker lands and **before** every tail re-append succeeds:

```
marker append  ✅ durable
tail[0] append ✅
tail[1] append ❌ store blip
tail[2] append ✅ (loop continues; parent may skip failed middles)
```

| Horizon | Behavior today |
|---------|----------------|
| **This turn** | Safe — in-memory view always includes the full kept tail (`maybe_compact` pushes every tail message into `view` regardless of append success). Covered by `partial_tail_reappend_failure_keeps_full_view_for_this_turn`. |
| **Next load** | **Unsafe residual** — `elide_before_latest_marker` keeps only root + latest marker + nodes **on the leaf path after the marker**. Unpersisted tail messages are gone from the **model-visible** history even though originals still exist **before** the marker on disk. |

So the marker commits the contract “old history is summarized,” but the verbatim tail may only be half-written. Elision then treats “missing tail” as intentional.

| Factor | Reality |
|--------|---------|
| Likelihood | Low if SessionStore appends are reliable; not zero under disk full, crash mid-loop, process kill |
| Blast radius | Lose up to the last K user turns from the model’s context (default K=3) — exactly the “keep verbatim” region |
| Summary still there | Yes — rolling summary may still mention those facts; recall degrades rather than hard-fails |
| Human-visible history | Full transcript still on disk; UI / search unaffected |
| Self-heal | None automatic; next compaction will not resurrect elided pre-marker originals into the model view |

**This is a documented residual of CH3, not a day-to-day UX bug.** Acceptable for merge of the architecture-hardening line if operators accept multi-append risk; closed by construction in the design below.

Related consequences of the rewrite model:

- **Duplicated tail on disk.** The kept tail exists twice within one conversation: the originals
  before the marker, the re-appended copies after it. This is *not* only a search concern — every
  reader that walks the raw leaf path sees both. Left unhandled it repeats the last
  `keep_recent_turns` turns in rendered chat history, doubles search hits, and — because fork /
  rewind resolves "turn N" by counting `Author::User` nodes — shifts turn indices, so forking at a
  given turn lands in the wrong place.

  **Mitigated, not removed:** the copies are authored `COMPACTION_TAIL_AUTHOR`
  (`liberado_conversation_store::COMPACTION_TAIL_AUTHOR`), and raw-path readers skip them via
  `Author::is_compaction_tail_copy()` — `ChatSessions::history`, `chat-search`'s scanner, and the
  session store's `turns()`. The model-visible view deliberately keeps them; they are what makes it
  a contiguous suffix. Guarded by
  `compaction_tail_copies_are_not_visible_in_rendered_history`. The duplication itself remains on
  disk until the viewport design below removes the copies by construction — so any *new* raw-path
  reader must remember to skip them, which is the standing cost this design retires.
- Multi-append atomicity is not free on an append-only DAG without multi-node transactions.

---

## 2. Target design: full log + side summary + viewport

### Intent (operator language)

- The **full conversation** remains one long append-only set of turns (no re-appended copies).
- The **summary** lives as its **own** node, separate from that spine.
- The summary **points at** the node the model should continue from (`continue_from`).
- The **conversation / session** points at the **current summary** (viewport root for the model).
- Context injection starts at the summary, then continues from the tail onto the real conversation through the current leaf.
- New turns keep appending to the **full conversation leaf** as they always have.

Compaction is a **lens**, not a second copy of recent history.

### Two graphs (keep them separate)

| Graph | Role |
|-------|------|
| **Storage graph** | Full conversation parent chain; leaf grows forever; truth for history UI, search, forks. |
| **Context / viewport graph** | Session → summary node → `continue_from` → walk to leaf. Used **only** when building model-facing messages. |

**Do not** reparent the live leaf under the summary. That would break “full conversation is one long thread” and make human history weird.  
“Point the conversation at the summary” means a **session/view pointer**, not “DAG parent of every new message.”

### Compact state (conceptual)

```text
CompactViewport {
  summary_node_id,   // content = rolling structured summary
  continue_from_id,  // first node of kept tail (user-turn boundary)
}
```

Optional later: chain of superseded summaries for audit (`summary_n` → previous summary) while the session only ever points at the **latest**.

### Context assembly

```text
messages = [
  system_root,                         // conversation create() root
  summary_as_system_or_special_role, // from summary_node content
  ... nodes from continue_from along the path to current leaf ...
]
```

On the next compaction:

1. Summarize (fold previous summary + newly elided region into a rolling update).  
2. Write a new summary node (or supersede content).  
3. Move `continue_from` forward to the new user-turn boundary.  
4. Point the session viewport at the new summary.

### Failure modes under the new model

| Failure | Behavior |
|---------|----------|
| Summarizer fails / empty | Do not update viewport → next turn still full history (fail-open) |
| Summary node write fails | Same |
| Viewport / pointer update fails after summary write | Orphan summary; still old viewport — harmless |
| Crash mid-compact | No “marker landed, tail missing” poison state |
| `continue_from` not on current leaf path (fork, corruption) | Ignore compact state → full history (fail-open) |

The multi-append partial-tail residual **goes away by construction**: there is no tail re-append.

---

## 3. Why this is better

1. **No multi-append atomicity problem** — one summary artifact + compact state update, not N tail clones.  
2. **No duplicated tail** — search and disk stay honest.  
3. **Full conversation stays “just the conversation”** — history, search, tooling keep a simple linear story.  
4. **Simpler reliability story** — viewport advances or it doesn’t; no half-written suffix.  
5. **Closer to peer practice** — LibreChat-style summary baseline / “index, don’t rewrite” rather than splice-and-clone.

---

## 4. Fit with the current codebase

CH3 today leans on **pure structure**:

- `Author::Named("compaction")` on the leaf path  
- `elide_before_latest_marker` on `leaf_path`  
- No session-level viewport field  

The re-architecture needs a **session-scoped compact cursor**:

| Placement | Pros | Cons |
|-----------|------|------|
| Side node + convention | Stays in the DAG | Need stable discovery of “latest summary for this session”; forks need care |
| Session metadata / root payload | Clear “conversation points at summary” | Store schema / API surface |
| Named node off root (`parent = root`, not on leaf path) | Discoverable | `leaf_path` won’t see it; load must fetch explicitly |

Existing seams that help: `Author::Named`, append-only `SessionStore`, `ChatSessions::load` / `maybe_compact` as the single injection point.

**Load path becomes smarter**: not “walk leaf_path and elide by author,” but “resolve viewport, then assemble.” Slightly more code; clearer invariants.

**Rendered history** must **not** start at the summary — humans still get the full thread. Viewport is **only** for model injection (same product intent as CH3’s history API behavior, cleaner split).

---

## 5. Design pressures and rules

### Hard rules

1. **Storage parent chain never reparents through the summary** — only the context assembler uses summary → `continue_from`.  
2. **Compact state is `(summary_id, continue_from_id)`**, updated only after summary content exists (best-effort atomicity: one metadata write after summary node is durable).  
3. **Any broken pointer → full history** (fail-open), same spirit as summarizer failure today.  
4. **System root always included** — summary replaces elided middle, not the system prompt.

### Fork / branch

If a session forks mid-conversation, the child must **clone or re-resolve** compact state so it does not share a live viewport with the parent incorrectly. Today’s marker-in-log forks “for free” because the marker sits on the path. Pointer model needs an explicit rule (e.g. copy `CompactViewport` into the forked session header).

### Migration from CH3 markers

Existing sessions with in-path markers:

- Dual-read for a while: if a compaction marker is on the leaf path, treat it as the viewport (summary content = marker message; `continue_from` = first node after marker).  
- New compactions write viewport state only (stop re-appending tails).  
- Optional one-shot: stop writing markers once viewport is present.

### Rolling summaries

Same product behavior as CH3: next summarizer input should see the previous summary so the roll is an update, not a fresh summary of only the newly elided slice. Implementation: previous summary content is either the current viewport’s summary node or the last in-path marker during migration.

---

## 6. Alternatives considered (not primary path)

| Approach | Notes |
|----------|--------|
| **A. Accept residual** | Document only (this doc’s §1). Cheap; residual remains. |
| **B/C. Incomplete-marker non-elision** | Small patch on CH3: don’t elide until full tail re-appended / commit flag. Mitigates but does not remove multi-append. Good interim if CH3.1 is far out. |
| **D. Single-node snapshot** | Put summary + serialized tail in one node. Kills multi-append; loses first-class message nodes for the tail unless carefully embedded. |
| **E. Repair on load** | If marker has fewer following nodes than expected, re-copy tail from pre-marker originals. Self-healing; heavier metadata. |

**Primary target: viewport / side-summary (this plan).**  
**Acceptable interim: B/C** if a small safety net is needed before CH3.1 lands.

---

## 7. Suggested implementation slices (when scheduled)

Unscheduled; order is indicative.

| Slice | Deliverable |
|-------|-------------|
| **S0** | Types: `CompactViewport`; store or session-header persistence; fail-open load helper |
| **S1** | `ChatSessions::load` assembles context from viewport when present; history API unchanged (full path) |
| **S2** | `maybe_compact` writes summary node + updates viewport; **stops** tail re-append for new compactions |
| **S3** | Fork / branch copies viewport; dangling `continue_from` → full history tests |
| **S4** | Migration: dual-read CH3 markers; optional stop writing markers |
| **S5** | Docs: update `context-compaction-plan.md` “as built”; failure-modes note; archive or mark this plan landed |

Tests should include: partial failure of summary write (no viewport advance); dangling continue_from; fork isolation; rolling second compaction; regression that history/search still see pre-viewport messages once.

---

## 8. Out of scope (unchanged from CH3 follow-ups)

Still separate from this re-architecture (see original plan’s “Deliberately not built”):

- Mid-turn precheck inside the executor loop  
- Between-turn tool-result pruning  
- Overflow-error-triggered retry compaction  
- Manual `/compact`  
- Dedicated summarizer model role  
- Goal-session / pack transcript compaction  

---

## 9. References

- Shipped design: [`context-compaction-plan.md`](context-compaction-plan.md)  
- Code: `crates/main-agent/src/compaction.rs`, `crates/main-agent/src/sessions.rs` (`maybe_compact`, `elide_before_latest_marker`)  
- Reliability doctrine: [`../spec/architecture/failure-modes.md`](../spec/architecture/failure-modes.md) §1–§2 (real store tests; opt-in guards are off)  
- Peer survey summary (OpenCode / Kilo / OpenClaw / LibreChat): same roadmap plan §“Outside research”
