# Delegate dogfood issues (2026-07-10 / session `01KX7AGD`)

Live dogfood of face-agent → `delegate` → dispatcher → subagent after the
subagent capability-gate fix (`ceiling ∩ allowed_mcps`). Session id
`01KX7AGD73QM27KGF2RSZ5EVAZ` (prefix `01KX7AGD`).

## Where to look

| Artifact | Path |
|----------|------|
| Chat transcript (face + compact tool results) | `.liberado/conversations/01KX7AGD73QM27KGF2RSZ5EVAZ.jsonl` |
| Subagent tool turns, budgets, guard downgrades | `.liberado/logs/liberado.err.log` (spans `orchestrate{… trigger="chat-delegate-…"}`) |
| Proposal notes (when written) | vault `proposals/proposals/<id>.md` |

There is **no** separate subagent session file. Subagent detail lives only in the
daemon log; the chat JSONL stores only the short string returned to the face
agent (`RESULT (…)`, `PROPOSAL: …`, or `tool error: …`).

---

## Issues

### D1 — Magnitude false positive: "clear" + "any" → HighConsequence proposal

**Status:** fixed (2026-07-10)  
**Severity:** high (blocks innocent read/list goals)  
**Seen on:** third `delegate` in `01KX7AGD` (`chat-delegate-01KX7AJQTTMZ7S5ZS7JAMM5KTH`)

**Symptom:** A simple "read Sarah task list / return a clean list" goal was
downgraded by the dispatcher guard to `Propose` with
`downgrade=HighConsequence`, even though `turbovault` is `consequence =
"reversible"` and the work was read-only.

**Root cause:** `is_sweeping_destructive` (`crates/common/src/capability.rs`)
requires:

1. A sweeping quantifier whole-word: `all`, `every`, `any`, …
2. A destructive stem prefix: `delet`, `clear`, …

The face goal+context contained both:

- **"any"** — "any other relationship task notes", "any tags"
- **"clear"** — "Provide a **clear** consolidated list" (adjective, not verb)

Stem match is `word.starts_with("clear")`, so the adjective `clear` counts as
destructive. Combined with `any` → false positive.

**Log evidence:**

```text
dispatch{… downgrade=HighConsequence}: dispatch decision downgraded by guard
  classified=DispatchSubagent confidence=0.9
orchestrate{action="Propose" … proposal_id=chat-delegate-01KX7AJQTTMZ7S5ZS7JAMM5KTH}
```

**Fix (landed):**

- `clear` removed from prefix stems; verb form only when followed by
  determiner/quantifier/particle (`clear the…`, `clear all…`).
- Sweeping words drop `any` / `each` (keep `all` / `every` / `entire` /
  `everything`).
- Regression test with the exact dogfood goal+context string
  (`clear_adjective_and_any_do_not_false_positive_read_goals`).

---

### D2 — Face `delegate` reports a proposal but never writes the file

**Status:** fixed (2026-07-10)  
**Severity:** high (user told to approve something that does not exist)  
**Seen on:** same Propose disposition as D1

**Symptom:** Face tool result:

```text
PROPOSAL: A high-consequence action needs human approval before it runs.
Proposal id: chat-delegate-01KX7AJQTTMZ7S5ZS7JAMM5KTH
Tell the human a proposal was drafted for their review (they approve it in the vault proposals folder).
```

No `proposals/proposals/chat-delegate-01KX7AJQ….md` (or any matching id) under
the vault. Follow-up subagent searched vault + chat-search and found nothing.

**Root cause:**

- `Orchestrator::run` `Propose` arm builds and signs a proposal **in memory**
  ("No vault write here — the daemon persists it").
- Non-face chat path (`ChatSessions`) has `write_chat_proposal`.
- Face path (`DispatchBridge::delegate` + `format_disposition`) only **formats**
  the Propose disposition with the id; it never persists the note.

**Fix (landed):**

- `DispatchBridge` carries `proposals_dir`; on `Propose`, writes
  `proposals_dir/proposals/<id>.md` and returns path in the face tool string.
- Write failure → `PROPOSAL_FAILED: …` (never claims a draft exists).
- Test: `format_propose_writes_note_and_returns_path`.

---

### D3 — Provider HTTP 400: incomplete tool_calls / tool messages

**Status:** fixed (2026-07-10)  
**Severity:** high (aborts orchestration mid-turn)  
**Seen on:** second and fourth `delegate`s in `01KX7AGD`

**Symptom:**

```text
tool error: orchestration failed: provider rejected request: HTTP 400:
{"error":{"message":"An assistant message with 'tool_calls' must be followed by
tool messages responding to each 'tool_call_id'. (insufficient tool messages
following tool_calls message)",…}}
```

**Likely area:** executor conversation assembly when a turn emits multiple
parallel tool calls, cycle detection/nudge, or error paths that drop some
`tool` role messages before the next provider call
(`crates/executor`, `crates/provider/src/openai_compat.rs`).

**Fix (landed):**

- Executor `run_loop` processes **all** tool calls in a turn and appends every
  tool-result message *before* doom/cycle escalations jump to the next provider
  call (or abort).
- Regression:
  `parallel_tool_batch_always_answers_every_tool_call_id_before_cycle_nudge`.

**Follow-up (session `01KX7BWV`, same HTTP 400):**

1. Dogfood ran against a **stale daemon binary** (built before D3 landed). Always
   rebuild + restart after executor fixes.
2. **Short cycle** is multi-tool name thrash only (`A,B,A,B`); requires ≥2
   distinct tools in the period (mono `AAAA` is not a cycle).
3. **Doom loop** owns mono-tool thrash: same tool + near-duplicate args.
   Differing identity fields (`path` / `file` / `note` / …) force similarity
   `0.0` so parallel multi-file `read_note`s are not near-duplicates (raw
   TF-IDF alone still scored shared `path`/`.md` tokens high).
4. Tests (hard-coded histories + mocked multi-call turns): different paths →
   neither guard; same path ×3 → doom not cycle; rephrased deepwiki still doom;
   ABAB/ABCABC still cycle.

---

### D4 — Subagent burns 8-turn budget on a simple filtered list

**Status:** mitigated (2026-07-10) — routing + preamble; budget dump still raw  
**Severity:** medium (QoL / reliability; partial success dumps noise to face)  
**Seen on:** `chat-delegate-01KX7AHYHFYNGGEFE4NCK97SZW`

**Symptom:** Goal was "retrieve Relationship/Sarah `#task` tasks". Subagent
used many turns (`get_vault_context`, `list_task_tags`, `list_tasks`,
`read_note`, repeated `search`, many `scratchpad_write`) and hit
`execution budget exhausted turns=8` → `PartiallySucceeded` with a huge truncated
call dump as the report summary. Face then re-delegated (which hit D1/D2).

**Contributing factors:**

- Open-ended "Relationship area" goal without a pointed tool plan.
- Scratchpad overuse relative to task size.
- Default subagent budget 8 still easy to spend without `submit_report`.
- Classifier may over-prefer `DispatchSubagent` for goals that could be
  `ExecuteDirect` with `turbovault:list_tasks` (+ filter).

**Fix (partial, landed):**

- Classifier prompt: simple vault/task list/filter/read → prefer
  `ExecuteDirect` + vault `relevant_mcps` (and seed when obvious).
- Subagent preamble: prefer smallest tool sequence; avoid long scratchpad
  loops when enough data exists to `submit_report`.
- Still open: cleaner budget-exhaustion summaries for the face (raw call dump).

---

### D5 — Observability: subagent trace not linked from chat

**Status:** open (docs / product) — deferred  
**Severity:** low  

**Symptom:** Debugging requires grepping daemon logs by `chat-delegate-*`
correlation; the chat JSONL has no link.

**Fix direction (later):**

- Include correlation id in face `RESULT` / `PROPOSAL` strings.
- Or persist a short "dispatch trace" sidecar / conversation node.
- Or `/session` UI deep-link to filtered log lines.

Not blocking correctness; track for TUI/ops maturity.

---

## Suggested fix order

1. **D1** — stop false proposals on read goals  
2. **D2** — if Propose still happens, user can actually approve  
3. **D3** — stop 400 aborts mid-mesh  
4. **D4** — cheaper happy path for list tasks  
5. **D5** — when dogfooding friction remains  

---

## Related prior fix (landed before this dogfood)

Subagent risk gate with empty `decision.capabilities` now derives
`ceiling ∩ allowed_mcps` (`subagent_gate_capabilities` in
`crates/orchestrator`). Earlier vault failures were capability empty-set, not
budget. Confirmed live on session `01KX7AE9` (`DispatchSubagent` +
`turbovault:*` succeeded).

---

## Checklist

- [x] D1 magnitude false positive  
- [x] D2 face proposal persistence  
- [x] D3 tool_calls HTTP 400  
- [x] D4 subagent turn waste / routing (prompt + preamble; dump polish later)  
- [x] D5 observability — model on spans; dispatch journals under `.liberado/dispatches/`  
- [ ] D4 follow-up: cleaner `PartiallySucceeded` budget summaries for face  

### D6 — CapabilityGap on vault list (session `01KX9S39`, 2026-07-11)

**Cause:** Classifier `ExecuteDirect` with high confidence, but `relevant_mcps` / `seed_calls`
used **non-catalog names** (likely bare `list_tasks` or tool-shaped names treated as MCP ids).
Guard `grants_mcp` only knows catalog MCP names like `turbovault` → CapabilityGap → Clarify.
**Not** missing policy grant (dispatcher already has `ExecuteMcp = "turbovault"`).

**Fix (landed):** `sanitize_decision_mcps` after classify — rewrite `mcp:tool` → `mcp`, drop
unknown bare names; empty `relevant_mcps` = full dispatcher grant. Log classified decision
fields. Delegation journals: `.liberado/dispatches/<chat-delegate-…>.jsonl` linked from tool
result footer (`parent chat` + journal path).

