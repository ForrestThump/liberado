# Liberado TUI — Maturity Audit & Revamped Roadmap

**Status**: audit 2026-07-10 · living roadmap  
**Crate**: [`crates/tui`](../../crates/tui/)  
**Peers (UX bar)**: Claude Code, Grok Build, OpenCode, KiloCode, VTCode  
**Related**: [`crates/tui/ARCHITECTURE.md`](../../crates/tui/ARCHITECTURE.md), [`crates/tui/ROADMAP.md`](../../crates/tui/ROADMAP.md) (historical), [`../spec/reference/api.md`](../spec/reference/api.md), goal sessions (`liberado-session` / `/api/goals*`), agentic mesh (`docs/spec/architecture/agentic-loops.md`), WebUI [`crates/webui`](../../crates/webui/)

**Non-negotiable:** Logical code stays **loosely coupled** so **TUI and WebUI share client logic** — only rendering shells differ. See §1.1.

---

## 1. Executive verdict

| Dimension | Today | Peer bar | Gap |
|---|---|---|---|
| **Architecture** | Solid thin client; Action/Effect; decomposed handlers/render | Thin client over daemon | Small |
| **WebUI reuse** | Partial (`chat-client-contract`, theme, markdown, commands) | Shared client core for chat + goals | **High** |
| **Chat usability** | Works: stream, tools chips, sidebar, themes, slash cmds | Fast chat + history | Medium |
| **Agent / goal UX** | **Absent** — no goal sessions, freeze, planner, verifiers | Goal mode, plan, cancel, live tools | **Critical** |
| **Coding agent UX** | Not a coding TUI (no diff, worktree, PR, gates) | Claude Code / OpenCode / Grok Build | **Critical** |
| **Speed / feel** | Acceptable; full redraw + full markdown reparse every frame | Sub-frame stream paint, zero jank | High |
| **Polish / density** | Functional, life-ops sidebar heavy | Command palette, dense HUD, status tokens | High |
| **Docs honesty** | ARCHITECTURE stale; ROADMAP “ALL DONE” masks product gap | Living status | Medium |

**Bottom line:** The TUI is a **capable Liberado chat + daemon monitor**, not a **world-class agentic terminal product**. The foundation (API client, SSE, state machine, themes, markdown) is good enough to build on. The missing half is **goal-session dogfooding**, **coding-agent density**, and **performance polish** — not another round of sidebar micro-features.

Do **not** treat historical ROADMAP “§9–10 ALL DONE” as product maturity. Those fixed real engineering defects. Peers compete on **agent loop visibility, speed, and daily-driver ergonomics**.

### 1.1 Coupling rule: one client brain, two shells

```
                    ┌─────────────────────────────┐
                    │  liberado-server (HTTP/SSE) │
                    └──────────────┬──────────────┘
                                   │
                    ┌──────────────▼──────────────┐
                    │  SHARED CLIENT CORE         │  ← no ratatui, no dioxus
                    │  chat-client-contract       │
                    │  + goal client / session VM │
                    │  + commands, theme, md AST  │
                    └──────┬──────────────┬───────┘
                           │              │
              ┌────────────▼──┐    ┌──────▼────────────┐
              │ liberado-tui  │    │ liberado-webui    │
              │ (ratatui)     │    │ (Dioxus/WASM)     │
              └───────────────┘    └───────────────────┘
```

| Layer | Lives in | Reused by | Must not contain |
|---|---|---|---|
| **Wire types + SSE decode** | `chat-client-contract` (extend) | TUI + WebUI + CLI | UI widgets |
| **Goal session client** | new module/crate e.g. `liberado-client-core` or expand contract | TUI + WebUI | ratatui / dioxus |
| **View-model / pure state** | shared: reduce `Action` → state, no I/O | TUI + WebUI | terminal raw mode |
| **Effects (HTTP spawn)** | thin adapters per shell *or* shared async helpers | both | render |
| **Render** | `tui/render/*`, `webui/components/*` | one shell only | business rules |

**Rules for every TUI phase below:**

1. **API calls and SSE mapping land in shared code first** — TUI only wires them to keys/draw.  
2. **State transitions stay pure** (Action in → new state + Effects out) — testable without a terminal.  
3. **WebUI must be able to import the same goal/chat reducers** without `crossterm`/`ratatui`.  
4. **No second copy** of `/api/goals` client in WebUI.  
5. Prefer **incremental extract** (grow `chat-client-contract` or a thin `liberado-client-core`) over a late big-bang rename — but **design seams now**, not after T4.

Already shared today (keep extending): `chat-client-contract`, `liberado-commands`, `liberado-markdown`, `liberado-theme`, `liberado-session` (types).

---

## 2. What exists today (strengths)

### 2.1 Architecture (keep)

```
Key/Mouse → handlers → App::update/handle_* → Vec<Effect>
EffectRunner → HTTP/SSE → Action channel → App::update → redraw
```

- **Thin client** over `docs/spec/reference/api.md` — correct mesh posture (Decision 2).
- **Shared contracts**: `chat-client-contract`, `liberado-commands`, `liberado-markdown`, `liberado-theme`.
- **Decomposed** `handlers/` + `render/` (~5k LOC + 1.7k tests in `app/tests.rs`).
- **Production hygiene** already landed: parking_lot, SSE timeout, backpressure channel, SIGTERM, message cap, mouse hit-testing.

### 2.2 Product features that work

| Feature | Notes |
|---|---|
| Chat stream | POST SSE: session/token/tool/tool_result/done/failed |
| Cancel / stop | Esc cancel, Ctrl+S stop (keep partial) |
| Conversations | List, load, filter, tree/collapse stubs |
| Slash commands | Shared `liberado-commands` (`/help`, `/theme`, `/new`, …) |
| Sidebar | Daemon status, reactions feed, conversation list |
| Markdown | Via shared crate |
| Themes | User themes + `/theme reload` |
| Mouse | Focus panes, scroll, double-click load |

### 2.3 Size snapshot (2026-07-10)

| Area | Lines (approx) |
|---|---|
| `app/tests.rs` | 1.8k |
| `app.rs` | 0.6k |
| `effects.rs` | 0.4k |
| `render/chat.rs` | 0.3k |
| Rest of crate | ~2k |

Tests are a real asset. The risk is **product surface lagging API surface** (`/api/goals*` exists; TUI ignores it).

---

## 3. Gap analysis vs peer class

Peers (Claude Code, Grok Build, OpenCode, KiloCode, VTCode) are not “pretty chat apps.” They are **agent harness UIs**: goal until done, tools live, budgets, diffs, cancel, resume.

### 3.1 Capability matrix

| Capability | Claude Code / Grok Build / OpenCode | Liberado TUI today |
|---|---|---|
| Conversational chat | ✓ | ✓ |
| Streaming tokens | ✓ | ✓ |
| Live tool timeline | ✓ dense | △ chips only |
| **Goal / agent mode** | ✓ first-class | ✗ |
| Plan before act | ✓ | ✗ (planner exists in backend only) |
| Verifier / CI-in-loop UI | ✓ (tests/lint as first-class) | ✗ |
| Criteria freeze / intake | rare | ✗ (backend ready) |
| Diff / file tree for coding | ✓ | ✗ |
| Multi-agent / subagent panes | ✓ advanced | ✗ |
| Cost / turn / budget HUD | ✓ | △ model name only |
| Command palette (fuzzy) | ✓ | △ slash only |
| Session picker + resume | ✓ | △ chat only |
| Permission / approval UX | ✓ | ✗ (Telegram elsewhere) |
| Image / file attach | often ✓ | ✗ |
| Offline / reconnect resilience | strong | basic reconnect toast |
| Startup time / paint jank | aggressively tuned | full redraw every poll |
| Vim / high-skill keybinds | optional | basic j/k |
| Accessibility / high-contrast | varies | theme tokens only |

### 3.2 Product identity mismatch

| What Liberado mesh is becoming | What TUI still is |
|---|---|
| Goal sessions, verifiers, intake freeze, PR factory, multi-domain packs | Chat + vault reactions monitor |
| Surfaces = clients of `/api/goals*` | Only chat + conversations + status |

Until the TUI **drives goal sessions**, dogfooding the agentic stack stays on CLI/tests/PR-dispatch — not the daily terminal.

### 3.3 Doc debt (audit findings)

| Doc | Issue |
|---|---|
| `ARCHITECTURE.md` “What’s still deferred” | Claims no stop key / weak DAG — **contradicts** ROADMAP (Ctrl+S, tree UI done) |
| `ROADMAP.md` | Reads “complete product”; ignores agent/goal/coding gap |
| `DECOMPOSITION.md` | `agent-tui-core` split is a **library strategy**, not UX maturity — schedule *after* product mode work |

---

## 4. Performance audit

| Hot path | Current behavior | Peer expectation | Priority |
|---|---|---|---|
| Draw loop | Poll input + redraw on every action; full frame | Dirty regions / skip redraw when idle | P1 |
| Markdown | Re-parse **every** assistant message every frame (`render/chat.rs`) | Parse once → cache `Vec<Line>`; invalidate on token append | **P0** |
| Streaming tokens | Append string, full re-layout | Incremental line append + scroll stick-to-bottom | P0 |
| Status poller | Periodic full `/api/status` + reactions | Adaptive interval; ETag / only on focus | P2 |
| Tool chips | New message per tool event | Collapsible tool group / timeline widget | P1 |
| Message cap | 500 sliding window | OK; virtualize render (only visible lines) | P1 |
| SSE | reqwest stream + decoder | OK; ensure cancel is instant (already abort handle) | P1 verify |
| Startup | Theme load + first paint | &lt;100ms to usable prompt | P2 |

**Speed target (north star):**  
- First paint &lt; 100ms after terminal enter  
- Token → pixel &lt; 16ms under steady stream  
- Esc cancel → “cancelled” &lt; 100ms perceived  
- No full markdown reparse on every token

---

## 5. UX quality audit

### 5.1 What feels half-baked

1. **Mode confusion** — always “chat”; no agent/goal mode chrome.  
2. **Sidebar is life-ops dense** (reactions) but thin on **agent progress** (turns, verifiers, budget).  
3. **Tool chips** don’t group into a turn; hard to scan long agent runs.  
4. **No empty-state guidance** when daemon down / no provider / coding pack absent.  
5. **No cost/token HUD** while streaming.  
6. **Slash commands** work but no **fuzzy command palette** (Ctrl+K / Ctrl+P class).  
7. **No visual hierarchy** for system vs tool vs assistant (peers use columns / collapsible trees).  
8. **ARCHITECTURE shortcuts table** incomplete vs actual keybinds (Ctrl+S, mouse, etc.).  
9. **No goal session list** despite server support.  
10. **Fork** command exists as stub while server support was “pending” — dead ends hurt trust.
11. **Network calls in the effect loop have no timeout** — the stop button is the one that hurts.
    See §5.1.1.

#### 5.1.1 The effect loop can block on an unresponsive daemon

**Open**, noted 2026-08-02 while reviewing the durable-turn TUI work (PR #30).

`EffectRunner` builds its HTTP client with `reqwest::Client::new()`, which sets **no default
timeout**, and several effects are `.await`ed inline in the event loop rather than spawned —
`select_model`, `fork_conversation`, `cancel_stream`, the `goal_action` family. While one is in
flight the TUI does not repaint or accept input.

For most of these it is a latency annoyance. For **cancel** it is a design inversion: Ctrl+S exists
for the case where the daemon is misbehaving, and that is exactly the case where the cancel POST
can hang. The surface that lets you escape a stuck turn is the one that freezes when the turn is
stuck. This became reachable when durable turns made cancel a network call at all — before that,
stopping was a local `AbortHandle::abort()` that could not block.

**Fix, in shape.** A timeout on the client covers the whole surface at once rather than each call
site separately:

```rust
reqwest::Client::builder().timeout(Duration::from_secs(30)).build()
```

`liberado-conformance`'s `DaemonClient` already does exactly this — the same defaulting mistake was
already found and fixed one crate over, which is the argument for the client-level fix rather than a
per-effect one. Streaming effects (`start_chat_stream`, `attach_conversation_stream`,
`join_goal_session`) must **not** inherit a total-request timeout: they are long-lived by design and
already bounded by `tuning::SSE_STREAM_TIMEOUT` per chunk. Either use `connect_timeout` on the shared
client, or hold a second client for streams.

**Not a regression**, so it did not block PR #30: `select_model` and friends have always behaved
this way. Written down because the cancel case makes it newly consequential, and because a
half-second stall on a healthy daemon hides how badly it degrades on a sick one.

### 5.2 Peer UX principles to copy (not clone)

| Principle | How peers do it | Liberado application |
|---|---|---|
| **One primary job** | Agent works until goal or stop | Default focus: **Goal session stream** when in agent mode |
| **Always escapable** | Esc / Ctrl+C clear semantics | Esc: cancel stream → clear input → blur; never ambiguous |
| **Show the harness** | Tools, budgets, permissions | Render `SessionEvent` kinds from `/api/goals/{id}/stream` |
| **Truth over vibes** | Tests fail visibly | Verifier pass/fail chips + `FAILURE_CLASS` repair feedback |
| **Keyboard first** | Palette + chords | Palette + mode-specific maps |
| **Thin client** | Local UI, remote brain | Keep — already correct |
| **Resume** | Sessions list | Chat + **goal sessions** dual lists |

---

## 6. Revamped roadmap (phased)

**Principles:**

1. Maturity = **agent dogfood + speed + density**, not more sidebar widgets.  
2. **Shared client core first** — every feature that is not pure paint is designed for **TUI + WebUI**.  
3. Server goal API is landed (`POST /api/goals`, SSE stream). Use it.

**Placement legend:** `S` = shared client core · `T` = TUI-only · `W` = WebUI-only · `B` = both shells later

---

### Phase T0 — Truth & baseline (0.5–1 day)

**Goal:** Honest docs + measurable baseline so work is not circular.

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T0.1 | Rewrite `ARCHITECTURE.md` deferred section; sync keybinds | T | Docs match code |
| T0.2 | Mark historical ROADMAP as “engineering backlog (done)”; point to this file | T | Single roadmap source |
| T0.3 | Manual perf checklist: startup, stream jank, cancel latency | T | Numbers recorded in this doc §10 |
| T0.4 | Screenshot / gif inventory of current panes | T | Before/after later |
| T0.5 | Document target crate map: contract vs client-core vs tui vs webui | S | Coupling rules accepted |

---

### Phase T1 — Performance foundation (3–5 days) — **P0**

**Goal:** Peer-class *feel* on chat before adding modes.

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T1.1 | **Markdown cache** at model layer: cache *AST or plain segments*, not ratatui `Line`s | S (+ T adapter) | Shared cache; TUI maps to `Line`s; WebUI maps to VNodes later |
| T1.2 | **Streaming buffer** pure model: append token → message state | S | Stick-to-bottom is shell policy |
| T1.3 | **Dirty flag** for TUI draw | T | Idle CPU near zero |
| T1.4 | **Virtualized chat** (viewport window) | T first; same index math usable by W | 500 messages smooth |
| T1.5 | Profiling hooks behind `LIBERADO_TUI_PROFILE=1` | T | Numbers for §10 |

**Coupling note:** Do **not** put `ratatui::text::Line` in shared crates. Cache intermediate representation (e.g. `markdown_to_lines` output as shared `MarkdownLine` — already in `liberado-markdown`) and let each shell theme it.

---

### Phase T2 — Goal session mode (5–8 days) — **P0 product**

**Goal:** TUI dogfoods agentic mesh; **WebUI can import the same goal client without rewrites**.

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T2.1 | **Shared** typed client: `start_goal`, `list_goals`, `get_goal`, `stream_goal`, `cancel_goal`, `list_domains` | **S** | No goals HTTP in `tui/api.rs` only |
| T2.2 | **Shared** goal session view-model: status, event log, composer draft | **S** | Unit tests without terminal |
| T2.3 | SSE → view-model apply for `session_started` / `tool_*` / `validation_*` / `session_finished` | **S** | Pure reducer tests |
| T2.4 | TUI mode enum `Chat` \| `Goal` + keybinds | T | Mode switch via `/goal` or chord |
| T2.5 | TUI goal list + timeline panes (render only) | T | List + select + new |
| T2.6 | TUI composer → shared `start_goal` effect | T→S | POST starts session |
| T2.7 | Cancel → shared `cancel_goal` | S + T wire | Esc cancels goal stream |
| T2.8 | Empty states (domains / provider) | S messages + T/W render | Clear operator messaging |
| T2.9 | wiremock tests on **shared client** | S | CI green; WebUI inherits |
| T2.10 | WebUI thin panel (optional same sprint) | W | List + stream using shared VM |

**Target module sketch (names flexible):**

```
crates/chat-client-contract/   # wire types (existing) + goal DTOs if not already in session
crates/client-core/            # NEW or expand contract: GoalClient, ChatStream, reducers
crates/tui/                    # ratatui only
crates/webui/                  # dioxus only
```

Prefer **`liberado-client-core`** (or grow `chat-client-contract` carefully) so WASM WebUI can depend without pulling `crossterm`/`reqwest` native-only bits — use traits for HTTP/stream transport (`native reqwest` vs `gloo`/`fetch`).

**UX sketch:**

```
┌─ Goal: file vault note… ─────────────┬─ Sessions ──────────┐
│ ● running · life · 4s                │  [+] New goal       │
│                                      │  ● life  file note… │
│ ▶ vault_list_notes  ok  3 notes      │  ✓ coding hello.txt │
│ ▶ vault_write_note  ok  written      │  ✗ life  force_fail │
│ ✓ validation  2 artifacts            │                     │
│ ■ succeeded: life-ops demo…          │  Domains: life,coding│
├──────────────────────────────────────┤                     │
│ goal> _                              │                     │
└──────────────────────────────────────┴─────────────────────┘
```

---

### Phase T3 — Intake / freeze / verifiers (4–6 days) — **P0 for quality**

**Goal:** Maker ≠ checker is *visible* and operable from **any** surface.

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T3.1 | Intake Q&A **state machine** (questions → answers → draft) | **S** | Headless tests |
| T3.2 | Freeze review model: criteria + verifier list edits | **S** | Produces payload for `start_goal` |
| T3.3 | TUI dialogs for intake/freeze | T | Answer → continue |
| T3.4 | WebUI forms for same models | W | Same JSON payloads as TUI |
| T3.5 | Verifier result view-model (`ok`, summary, `FAILURE_CLASS`) | **S** | Both shells render chips |
| T3.6 | Attach `goal_contract` JSON to coding goals | **S** | Matches PR-dispatch freeze |

Depends on server endpoints for intake if not already HTTP-exposed; until then shared client assembles pre-frozen contract into `GoalSpec.payload`.

---

### Phase T4 — Coding-agent density (6–10 days) — **peer parity core**

**Goal:** When domain=coding, feel like OpenCode/Claude Code — not a chat with file chips.  
**Shared:** file list + timeline + budget **models**. **Shell-specific:** diff paint (terminal vs HTML).

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T4.1 | File change list model from artifacts / events | **S** | Side panel data |
| T4.2 | Unified diff **text** model + optional fetch API | **S** | TUI Paragraph / WebUI `<pre>` |
| T4.3 | Turn / attempt / role HUD model | **S** | Both status bars |
| T4.4 | Budget display (turns, wall time) | **S** | Always visible while running |
| T4.5 | Collapsible tool timeline model (group by turn) | **S** | Long runs scannable |
| T4.6 | Open path in `$EDITOR` | T | `e` on file row |
| T4.7 | Open path / download in browser | W | Parallel affordance |
| T4.8 | Optional PR URL card when factory notifies | **S** + shells | Stub OK |

**Out of scope for T4:** in-surface merge, git commit (factory owns publish).

---

### Phase T5 — Command palette & power-user UX (3–5 days)

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T5.1 | **Command registry** + fuzzy filter (pure) | **S** | Same cmds for TUI + WebUI |
| T5.2 | TUI palette UI (Ctrl+K) | T | Sub-100ms open |
| T5.3 | WebUI command palette / slash menu | W | Same registry |
| T5.4 | Keybind help from registry | **S** + T overlay | Always accurate |
| T5.5 | Vim-lite / clipboard / paste | T (platform) | No crash on huge paste |

---

### Phase T6 — Reliability & multi-session (4–6 days)

| # | Work | Where | Exit criteria |
|---|---|---|---|
| T6.1 | Reconnect / resume goal from snapshot | **S** | No silent death |
| T6.2 | Multi-session store (active id + map) | **S** | Switch without killing |
| T6.3 | Notification model (goal finished) | **S** | Toast / badge per shell |
| T6.4 | Proposal / draft-PR visibility | **S** if API | Or link to Telegram |
| T6.5 | Offline / retry queue for GETs | **S** | Clear degraded mode |

---

### Phase T7 — Client-core hardening (continuous, not a late big-bang)

Historical `DECOMPOSITION.md` imagined `agent-tui-core` **after** everything. **Revised stance:** extract **as we build T2–T6**, not as a final rewrite.

| # | Work | Exit criteria |
|---|---|---|
| T7.0 | **From T2 day one:** goals HTTP + reducers in shared crate | WebUI can depend |
| T7.1 | Transport traits: `HttpClient` / `SseByteStream` (native vs WASM) | Both shells |
| T7.2 | Move chat stream apply logic next to goal reducers | One brain for chat+goals |
| T7.3 | Optional rename to `liberado-client-core` when layout stabilizes | Docs + examples |
| T7.4 | Example: WebUI goal panel &lt; 200 LOC shell over shared VM | Proof of reuse |

**Do not** invent a third agent framework for the browser.

---

### Phase T8 — Peer polish pass (ongoing)

| # | Work | Where |
|---|---|---|
| T8.1 | Motion / spinners / progress | shells |
| T8.2 | Sound optional | shells |
| T8.3 | High-contrast theme tokens | **S** theme + shells |
| T8.4 | CI snapshot tests | T (and W if cheap) |
| T8.5 | First-run wizard | **S** steps + shells |

---

## 7. Recommended sequencing (critical path)

```text
T0 truth + coupling map
  → T1 perf (shared md segments + TUI dirty draw)
  → T2 goal client in SHARED crate, then TUI shell   // dogfood + WebUI-ready
  → T3 freeze/verifiers models shared, dual shells
  → T4 coding density models shared, dual paint
  → T5 command registry shared, dual palette
  → T6 multi-session shared
  → T7 continuous extract / transport traits
  → T8 polish
```

**Do not** prioritize:

- More reaction-feed chrome  
- **Duplicating** goals/chat logic in WebUI “for speed”  
- Premature widget frameworks  
- VTCode-style mega-tool UI  
- Embedding agent logic in either UI  

---

## 8. Success metrics (definition of “mature”)

| Metric | Target |
|---|---|
| Daily dogfood | Author uses TUI for ≥50% of Liberado goals (not just chat) |
| Goal mode | Life + coding goals startable, streamable, cancellable from TUI |
| **WebUI reuse** | Goal list + stream works in WebUI **without** a second HTTP/SSE implementation |
| **Shared tests** | ≥80% of client logic coverage lives outside `tui`/`webui` crates |
| Stream jank | No visible full-history reparse stutter during 30s stream |
| Cancel latency | Esc → cancelled UI &lt; 100ms |
| Peer checklist | §3.1 matrix: all **Critical** rows at least △→✓ |
| Crash rate | No panic on disconnect / huge paste / 500 msgs |
| New contributor | Can map shared VM vs shell render in &lt;30 min from ARCHITECTURE |

---

## 9. Work estimates (calendar, 1 strong engineer)

| Phase | Days | Cumulative |
|---|---|---|
| T0 | 1 | 1 |
| T1 | 4 | 5 |
| T2 | 7 | 12 |
| T3 | 5 | 17 |
| T4 | 8 | 25 |
| T5 | 4 | 29 |
| T6 | 5 | 34 |
| T7 | 10 | optional |
| T8 | ongoing | — |

**~5–6 weeks** to peer-competitive **agent dogfood** (T0–T4). Palette + multi-session push toward “daily driver” (T5–T6).

---

## 10. Perf baseline log (fill in T0.3)

| Probe | Before | After T1 | After T4 |
|---|---|---|---|
| Cold start to prompt | _ | _ | _ |
| 60s stream CPU % | _ | _ | _ |
| Markdown parse ms / frame | _ | _ | _ |
| Esc cancel ms | _ | _ | _ |

---

## 11. Relationship to mesh roadmap (A–F)

| Mesh slice | TUI dependency |
|---|---|
| A–E (loop, freeze, verifiers, curriculum, draft PR) | Backend ready; **TUI must surface them (T2–T3)** |
| F session SSE | **Consumed by T2** — already on server |
| PR-dispatch | Optional status cards (T4.7 / T6.4), not in-TUI forge |
| Heuristics-tuner proposals | Link/open `PROPOSAL.md` path; no auto-apply |

---

## 12. Conversation titles (design — partial now)

**Status:** first-line default **shipped**; agent flash-title and `/title` slash **deferred**.

| Writer | When | Mechanism |
|---|---|---|
| **First-line default** | Header `title` is `None` on first user turn; also lazy backfill on `list` | `liberado_main_agent::default_conversation_title` → `ChatSessions::set_title` / store |
| **Flash-title agent** (future) | Cold-start / after N turns / on demand | Cheap model reads history → `set_title` (same API). Must **not** run on every token. |
| **`PATCH /api/conversations/{id}`** | Already exists | `{ "title": "..." }` — used by agents and tooling |
| **`/title …` slash** (future) | User renames when direction drifts | Shared `liberado-commands` → client effect → `PATCH` |

**Rules:**

1. Title is **display-only** (store comment: derived/regenerable; not source of truth).  
2. Default seed runs **only when `title` is `None`** — never clobbers agent or user renames.  
3. Agents and slash always go through `set_title` / `PATCH` (overwrite OK).  
4. TUI/WebUI must not invent a second title store; they render `ConvHeader.title`.

**Not building now:** flash agent pack, `/title` command, auto-retitle heuristics.

---

## 13. Immediate next implementation slice

When coding resumes, start with:

1. **T1.3** TUI dirty draw (idle CPU)  
2. **Slash command palette** (progressive filter as you type `/`)  
3. **T0.5 + crate sketch** — decide `liberado-client-core` vs expand `chat-client-contract`  
4. **T2.1–T2.3 shared goal client + reducers first**, then TUI panes (T2.4–T2.7)  
5. **WebUI goal panel** as soon as shared VM exists (even minimal) — forces the coupling discipline  

Do **not** implement goals only inside `crates/tui/src/api.rs`.

---

## 14. Appendix — peer notes (what not to copy)

| Peer pattern | Risk | Liberado stance |
|---|---|---|
| Mega unified tools (VTCode) | False success, opaque UI | Discrete tools + event names |
| Local agent in TUI process | Breaks daemon-first | Stay HTTP client |
| Auto-apply prompt/config | Decision 14 | Never |
| Infinite scroll of raw JSON | Noise | Timeline + collapse |

---

**Document owner:** TUI / surfaces.  
**Review after:** T2 complete (first goal dogfood) and after T4 (coding density).
