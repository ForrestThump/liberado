# Session focus & interactive goal sessions — plan

**Status**: plan, 2026-07-12 — no code yet.  
**Decision already made**: the "call transfer" to a specialist is **not a new process or daemon**.
It is the UI moving its *input focus* onto an interactive goal session running on the same hub,
same daemon, same wire contract. Transcripts are **separate but linked** (the same pattern
delegation already uses: dispatch journals + correlation ids).  
**Related**: [`agentic-loops.md`](../architecture/agentic-loops.md) ·
[`channels-and-interactivity.md`](../architecture/channels-and-interactivity.md) (D7 interactivity + the three channels) ·
[`contracts.md`](../architecture/contracts.md) · [`api.md`](../reference/api.md) ·
[`verifiers.md`](../architecture/verifiers.md) §3 (intake) ·
[`delegate_dogfood_issues.md`](delegate_dogfood_issues.md) ·
[`architecture-alignment-audit-2026-07-11.md`](architecture-alignment-audit-2026-07-11.md)

---

## 1. The interaction being built

Today (stays exactly as is): *"what's the weather / any news?"* → face agent → `delegate` →
dispatcher routes an MCP query → report comes back → face agent relays it. One conversation, the
human never leaves the main agent.

New: *"I want to build a program that does XYZ"* → dispatcher recognizes an **interactive** goal →
a specialist goal session is spawned (coding pack, probably opening in its **intake** phase) → the
UI **offers** the switch:

```
▸ coder session started: "todo CLI with file store" (g_01ABC…)
  [Enter] join session   [Esc] stay here
```

Human accepts → the same input box now feeds the specialist session; the transcript pane renders
that session's event stream; a chip shows the active hat (`▸ coder g_01ABC`). `/back` (or the
session reaching a terminal state) returns focus to the main conversation, which receives a
**summary artifact** linking to the full session transcript. The human can rejoin any live session
from the session browser, or start a fresh main conversation — the generalist never went away;
they just stopped talking *through* it for a while.

Why this shape and not a specialist daemon: the daemon is where the guarantees live (capability
narrowing, conversation store, provenance, proposal gates, one control plane — Decision 2). A
per-specialist daemon would duplicate policy/keys/stores per hat and reintroduce the multi-daemon
topology the agent-pools research rejected. Process isolation, when a session needs it, is a
**pack implementation detail behind the hub** (`liberado-coder-run` is already that adapter), and
future remote execution is a *RemotePackRunner* proxying to another Liberado over the same
HTTP/SSE contract — the UI stays single-homed forever.

## 2. Settled design decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | Focus switch, not connection transfer | UI keeps one connection to one daemon; all guarantees stay in one place |
| D2 | Separate-but-linked transcripts | Mirrors dispatch journals; keeps the main conversation's context lean (token-efficiency pillar); correlation id stitches them |
| D3 | Explicit consent to switch | The UI offers; the human accepts. No surprise yanking of the input box |
| D4 | Authority is unchanged by joining | The session runs under dispatcher-derived capabilities (`ceiling ∩ allowed`); a human typing into it never widens anything; proposals still gate high-consequence acts |
| D5 | Additive wire changes only | The converged `SessionEventKind` vocabulary (2026-07-11) is the substrate; this plan adds variants, renames nothing |
| D6 | Hats are config, not code | A "hat" = domain pack + role/prompt/model overrides + capability component — a topology entry, like MCPs and pools |
| D7 | **Everything is one `Session`** (2026-07-12) | "Conversation" and "goal session" are the same concept; the differences are **attributes, not subtypes**: `goal: Option<Goal>` (presence ⇒ run-to-terminal; **terminality = `goal.is_some()`**), `origin` (human / offer / cron / delegate), `visibility` (foreground vs background — subagents are just `delegate`+background), `runner`/`domain` (the "type" chip: Primary/Coding/Life/…). The converged wire vocabulary already committed to this; only identity+storage stay split for now. **Staged**: adopt the model + the unified `/sessions` surface now (S3); converge the two stores into one durable `Session` store as its own slice (S5′, below). See `memory/project_unified_session_model.md`. |

### The unified `Session` model (D7) — how it lands per slice

- **S3 (now)**: the `/sessions` switcher is **one session list** — the primary chat is the goal-less
  `Session` at the top; goal sessions are rows beneath it, each with a **kind chip** (`SessionKind`:
  Primary/Coding/Life/…, derived from `domain`) and a goal-status column. The two stores
  (`conversation-store`, `GoalSessionHub`) stay separate behind a thin surface adapter; the UI
  already embodies "everything is a Session."
- **S5′ (its own later slice — store convergence)**: merge `conversation-store` + `GoalSessionHub`
  into one `Session` store — `goal: Option<Goal>`, durable node-graph JSONL transcripts (the S5
  durability work, done under the unified name), `origin` links, and cron/hook/subagent runs folded
  in as background sessions. This is where the "no duplication of responsibility" payoff actually
  lands; S3's adapter is the seam it replaces.

## 3. What already exists (this plan is mostly composition)

| Piece | Where | Role in this plan |
|---|---|---|
| One event vocabulary for chat + sessions | `liberado-session::SessionEventKind` + wire mirror in `chat-client-contract` | The transcript pane renders both with one decoder — the prerequisite this plan was waiting on |
| Goal session kernel | `liberado-session` (`GoalSpec`, hub, store, `DomainPackRunner`) | The specialist *is* a session |
| Session API | `/api/goals*` + SSE stream (history catch-up + live) | Joining = subscribing; nothing new to invent for reads |
| Coding + life packs | `CodingSessionPack`, `LifeOpsDemoRunner` | First interactive hat + the second-domain test bed |
| Criteria intake | `coder-core::intake` + `coder-agent::intake_session` (typed `IntakeOutcome`, clarify rounds, freeze) | Becomes phase one of an interactive coding session — it *already needs* exactly the human-input channel this plan adds |
| Delegation linkage | face agent `delegate` → dispatch journals under `<LIBERADO_DATA_DIR>/dispatches/`, correlation ids | The separate-but-linked precedent; the offer/return handoff extends it |
| Conversation persistence | `conversation-store` (append-only JSONL, parent links for forks) | The pattern the session transcript log copies |
| TUI session browser | `/session` browser, sidebar, slash palette | The rejoin surface |
| Named authority | pools / `Grant.component` in `policy.toml` | The capability half of a hat profile |

## 4. Gap analysis

### G1 — Kernel: inbound human input (the one genuinely new primitive) — ✅ done (S1, 2026-07-12)

Landed in `liberado-session`: `HumanInput`, `InputChannel` (bundles the receiver + kernel idle
budget, `recv()` → `InputOutcome::{Received,IdleExpired,Closed}`), the `DomainPackRunner::run`
signature gained `inputs: InputChannel` (coder pack ignores it; `LifeOpsDemoRunner` grew an
interactive branch), `GoalSpec.max_idle_secs`, `GoalSessionRecord.awaiting_input` (store-derived,
and cleared on terminal — a finished session is never awaiting), `GoalSessionHub::send_input`
(echoes `HumanInput` into the transcript, fails cleanly on unknown/finished). Wire mirror +
server SSE + client match arms updated in lockstep. **Deferred to S4**: the `origin` link field
(it's linkage, not the input primitive). Original design notes retained below.

`DomainPackRunner::run` today is fire-and-run-to-terminal: it gets an event `Sender` and a cancel
watch, nothing inbound. Needed in `liberado-session`:

- A per-session **input channel**: `GoalSessionHub::send_input(session_id, text)` delivering into
  a receiver the runner gets. Trait change (breaking, two impls + tests):
  `run(&self, session_id, goal, events, inputs: mpsc::Receiver<HumanInput>, cancel)`.
  Non-interactive packs ignore the receiver — no behavior change.
- Two additive `SessionEventKind` variants:
  - `AwaitingInput { prompt, options }` — the pack is blocked on the human; surfaces render the
    prompt state (and the TUI knows the input box is "hot"). `options` covers intake's
    multiple-choice questions.
  - `HumanInput { text }` — every accepted input echoed into the event history, so the session
    transcript is complete and replayable on its own.
- **Idle budget**: an interactive session waiting on a human is not "stalled work," but an
  abandoned one must still die — a `max_idle_secs` knob (kernel budget, not pack knob) that
  terminates to `BudgetExhausted` with the wait duration in the summary.
- `GoalSessionRecord` gains `awaiting_input: bool` (derived, for list/snapshot views) and an
  `origin: Option<SessionOrigin { conversation_id, correlation_id }>` link field (D2).

### G2 — Server + wire: the input endpoint — ✅ done (S2, 2026-07-12)

- ✅ `POST /api/goals/{id}/message` `{"text": "..."}` → `hub.send_input`. `send_input` now returns
  a typed `SendInputError` (`Unknown` / `Terminal` / `Closed`) so the handler maps cleanly to
  **404 unknown**, **409 terminal** (`Closed` — a teardown race — also 409), **202 accepted**.
  (Cancel already exists.)
- ✅ Wire mirror (`chat-client-contract`): `AwaitingInput` / `HumanInput` landed additively in S1.
  `SessionOffered` (G3) is still pending. Old clients treat unknown kinds as no-ops by design —
  additive is safe.
- ✅ `api.md`: endpoint row + interactive-session event note added.
- HTTP integration tests (hooks-test pattern, against a real `axum::Router` + life pack): message
  delivered → 202 + `human_input` echoed + session drives to `Succeeded`; unknown → 404; finished
  → 409.

### G3 — Launching an interactive session (revised 2026-07-12)

> **Superseded framing**: the original G3 had the *dispatcher classify a goal as interactive*.
> [`channels-and-interactivity.md`](../architecture/channels-and-interactivity.md) Decision A
> retires that — the dispatcher can't know whether the human wants to babysit, so it doesn't try.
> Interactivity is a **capability** (`ask_human`) × **visibility** (fg/bg) × **budget**, not a
> classification. Two launch paths remain, both human-gated:

- **`/spawn <domain> <goal>` (primary)**: a human explicitly starts a foreground session with
  `ask_human: on` and `origin` = the current conversation. No dispatch-side heuristic.
- **The offer (`SessionOffered`, secondary)**: a chat turn where a human *is* present and whose
  `delegate` result decides the work is better done hands-on — the chat stream emits
  `SessionOffered { id, domain, description }` and the surface renders a `/join` affordance. Still an
  offer the human accepts, never an auto-switch.
- **`ask_human` capability**: a component-style grant on the session / hat profile — whether the pack
  *may* emit `AwaitingInput`. Background (cron/dispatch-spawned) sessions run with it off, or on with
  questions queued against `max_idle_secs`.
- **Linkage**: the spawned session's `origin` carries the conversation id (+ correlation id); the
  dispatch journal entry gains the session id. Both directions navigable.

### G4 — TUI: the focus model

- `enum InputTarget { Conversation, GoalSession(id) }` on `App`; input routing sends either a chat
  turn or `POST /api/goals/{id}/message`.
- Joining subscribes to `/api/goals/{id}/stream` (history catch-up gives the full specialist
  transcript on join/rejoin).
- Renderers for the session-only kinds the chat view currently no-ops (`role_started`, `progress`,
  `validation_finished`, `loop_guard`, `awaiting_input`) — status lines / chips, not walls of text.
- The offer affordance (D3), the hat chip, `/back`, and `/join <id>` (manual path via the session
  browser, which also makes S3 testable before the offer exists).
- On `session_finished` while focused: render the terminal summary, flip focus back to the parent
  conversation automatically.

### G5 — Persistence + the return handoff

- ✅ **Durable transcripts (S5, done 2026-07-12)**: `GoalSessionStore::open(dir)` backs each session
  with an append-only `<dir>/goal-sessions/<id>.jsonl` log (`start` record line, one `event` line
  per event, `status`/`finish` lines), rehydrating list/snapshot views on boot. In-memory
  (`::new()`) stays the default for tests. A non-terminal session in a replayed log is coerced to
  `Failed` (no pack runs it post-restart; packs aren't resumable yet — the transcript is view-only).
  The daemon wires `<LIBERADO_DATA_DIR>/goal-sessions`. Same Decision-12/17 reasoning (operational
  data outside the vault; conversation-store is the pattern).
- ✅ **Return handoff (S4, done 2026-07-12)**: `SessionOrigin`/`origin` on `GoalSpec`; when a session
  carries an origin, the server spawns a watcher (`spawn_return_handoff`) that, on terminal, appends
  a compact summary node (kind · status · outcome · artifacts · `/join` hint) to the parent
  conversation via `ChatSessions::append_note`. Race fix: it waits for the *record* to settle
  terminal (not just the finished event) so status/result are populated. Best-effort (no chat / bad
  id / append error is logged, never fatal). End-to-end test: spawn → answer → summary folded in.
- **Return handoff**: when a session with an `origin` reaches terminal, append a summary message
  to the parent conversation (`GoalResult.summary` + artifacts + session id link) — the same
  "report folds back into the conversation" shape `delegate` already produces, so the main agent
  can discuss the outcome next turn without carrying the whole specialist transcript (D2).

### G6 — Hat profiles (config)

- `[[session_profiles]]` (name TBD — "hats" informally) in `topology.toml`: `name`, `domain`
  (pack), `component` (capability grant key, like pools), and an **opaque** pack-overrides section
  (role/model/prompt) the pack parses itself — same rule as `[tuning.coder]`, keeping the config
  stack pack-agnostic.
- Dispatcher/face agent can name a profile when spawning; the TUI session browser can offer
  "new session as <hat>".

### G7 — Intake as phase one of an interactive coding session

- Wire `intake_session`'s clarify loop through `AwaitingInput`/`HumanInput` instead of its current
  headless path: questions render as prompts, answers flow back, `ReadyForFreeze` renders the
  draft contract with accept/edit — the freeze UI `verifiers.md` §3.7 asks for, landing in the TUI
  for free once G1–G4 exist.

### G8 — Invariants (tests to write alongside)

- Layer rules: untouched — no new crates, no new deps below the wire contract (TUI still depends
  only on client-tier crates; `session` still depends only on `common`).
- Capability non-widening: a `send_input` into a running session must not alter the session's
  `CapabilitySet` — assert in a hub test; later an eval scenario (UNSAFE-acts must never increase).
- One-writer rule: input while the pack is mid-model-turn is **queued**, delivered at the next
  await point (never interleaved into a provider call).

## 5. Slices (each ships + is verified before the next)

| # | Slice | Proves it with |
|---|---|---|
| ✅ S1 | Kernel input channel + `AwaitingInput`/`HumanInput` + idle budget (`liberado-session`; trait change ripples to both packs) — **done 2026-07-12** | Unit tests (green): interactive `LifeOpsDemoRunner` asks → awaits → echoes; idle-budget → `BudgetExhausted`; `send_input` to finished session errors |
| ✅ S2 | `POST /api/goals/{id}/message` + `api.md` (wire variants landed in S1) — **done 2026-07-12** | HTTP integration tests (green) against a real router (the hooks-test pattern): 202 + echo + drives to success; 404 unknown; 409 finished |
| S3 | TUI focus MVP: **unified `/sessions` switcher** (one list — primary + goal sessions, kind chips + goal-status) · `/join <id>` · `/back` · session-kind renderers | Live smoke: full Q&A with the life demo through the TUI |
| ✅ S4 | The offer + return handoff — **done 2026-07-12**. `SessionOrigin`/`origin` on `GoalSpec`; `SessionOffered { id, domain, description }` wire variant (all clients decode; TUI renders a joinable `/join` affordance, retained for the human-attended offer). **Trigger = `/spawn <domain> <goal>`** ([`channels-and-interactivity.md`](../architecture/channels-and-interactivity.md) Decision A — no dispatcher interactivity classification): the TUI POSTs `/api/goals` with `origin` = current conversation + `payload.interactive`, then auto-joins. **Return handoff**: a server watcher folds the terminal summary into the parent conversation (`ChatSessions::append_note`). Still pending: the `ask_human` capability flag (a documented future grant, slots into S6). | Tests (green): `/spawn` parse→effect→focus; `SessionOffered` decode + offer render; `append_note`; `format_handoff_note`; **end-to-end** POST-with-origin → answer → summary folded into a real conversation. |
| ✅ S5 | Durable session transcripts (append-only JSONL per session) + rehydrate on boot — **done 2026-07-12** | Store unit tests (green): reopen rehydrates a finished session's record + events; a non-terminal session coerces to `Failed`; in-memory store persists nothing. Live: restart daemon → `rehydrated sessions=1` → `GET /api/goals` shows the finished session + full transcript |
| ✅ S5′ | **Store convergence (D7)** — **done 2026-07-13**. **(1)** The kernel got a store *seam*: `GoalSessionHub` holds `Arc<dyn SessionRecordStore>`, not a concrete store — forced by the layer rule that the kernel may not know what a provider `Message` is, which is why the converged store lives *above* it and reaches down. **(2)** New `liberado-session-store`: one dir, one id space, one append-only JSONL log **per session**, holding both message nodes (the DAG) and pack events. `SessionHeader` is the converged record; **`goal: Option` is the entire difference** (terminality *is* `goal.is_some()`), with `origin`/`visibility`/`grant` as attributes. Two typed lenses over one engine: `ConversationStore` (sees every session) and `SessionRecordStore` (sees only goal-bearing ones — `GoalSessionRecord` cannot express a goal-less session). **(3)** Chat *and* the goal hub now run on that one store. **(4)** `GET /api/sessions` returns the one list, and the TUI switcher reads it — the client no longer polls two endpoints and stitches them, and Enter branches on `has_goal()` rather than a row type. **(5) Unattended work stops firing into the void.** A cron, a webhook/hook, and a `delegate`d subagent are now recorded as `Visibility::Background` sessions — the *same* rows as your chats, running while they run, terminal when they're done, with a transcript. `Visibility` moved **into the kernel** and onto `GoalSessionRecord`: the store had been hardcoding `Foreground` on `insert`, because the very lens every non-human trigger writes through could not carry the field — the type existed and nothing could emit it. `SessionOrigin.conversation_id` became optional, since a cron has a dispatch correlation but **no parent conversation** (a subagent has both, and hangs off the chat that asked for it). These are *recorded*, not *hosted*: the dispatcher/orchestrator still runs them, so joining one is read-only. | Tests (green, 93 suites): a cron firing is one background session, tied to its journal entry, with the decision narrated into its transcript; a reaction that needed a human **fails** rather than reporting success, carrying the unanswered questions; an attached-but-failing orchestrator is **not** reported as a missing one; a delegation is a child of its chat; the switcher marks a session nobody started. **Live** (real daemon): a chat and a goal session side by side in one directory; `/api/sessions` interleaves both with parent edges + awaiting flags; the S4 return handoff still folds a summary into its parent chat — now across one store, not a bridge between two; **a real cron fired → a `background` row appeared with a readable transcript → survived a restart**. |
| ✅ S6 | **Session profiles** in topology + `/spawn <profile>` — **done 2026-07-12**. Naming settled (open question #5): **"profile"** everywhere (config `[[session_profiles]]`, `SessionProfile`, UX) — "hat" stays informal prose only. `[[session_profiles]]` = `name` · `domain` (pack) · `component` (grant key, defaults to `name` — the pool rule) · opaque pack-parsed `overrides`. **Goal sessions now have an authority boundary at all**, which they previously did not: `SessionGrant { capabilities, profile, overrides }` is resolved by the *server* from config (the kernel never reads config), recorded on the session, and never widened. **`Capability::AskHuman` closes the S4 leftover and makes Decision A enforceable**: a grant without it gets a *closed* input channel, so the pack cannot block on a human — and `POST /message` answers **403** (never allowed), distinct from 409 (too late). Packs check capabilities at the point of the act (`ctx.can(&Capability::Write(zone))`). | Tests (green, 88 suites): profile→pack+narrower-grant resolution; unknown profile falls back without inventing authority; no grant ⇒ zero authority; dup/typo'd component fail validation; **G8 non-widening** (`send_input` never widens a grant); AskHuman-less session never awaits *despite* `interactive: true`; 403≠409; write refused at the act. **Live** (real daemon): `/spawn research` → resolves to the **life pack** with read-only caps → succeeds **without ever awaiting**, and `POST /message` → 403. |
| ✅ S7 | **Intake-first coding sessions** — **done 2026-07-13**. `CodingSessionPack` now runs `verifiers.md` §3.4 before writing a line: `run_intake` → clarifying questions through the S1 human-input channel → **draft contract rendered for review** (criteria + the machine gates it will be judged against) → `accept` / `reject` / *any other text = a revision fed back to intake* → `GoalContract::freeze(…, FreezeAuthority::Human)`. Bounded by `max_clarify_rounds` (§3.4 step 5 → `NeedsReview` with the partial draft). Gated on `AskHuman` (S6): an unattended session skips intake and builds directly, rather than blocking on a human who isn't there. Knobs come from the S6 opaque `overrides` (`[session_profiles.overrides] intake.*`), with `payload` winning. **The payoff: the frozen contract supplies the `verifiers`** — this pack previously ran with `verifiers: []`, i.e. grading its own homework. | Tests (green): clarify→answer→draft→accept freezes a contract *carrying gates*; reject builds nothing; **free text is a revision, not an accept** (`"add a test…"` must not prefix-match `accept`); round budget → `NeedsReview`; payload beats profile overrides; multi-line contract renders line-by-line in the TUI. **Live** (deepseek, real daemon): `/api/goals` coding + interactive → intake drafted a real contract (expanded `rust-strict` into `cargo test`/`clippy -D warnings`/`fmt --check`) → `accept` → **"contract frozen (4 verifiers)"** → coder ran and **all 4 gates actually executed**. |

S1–S3 are the spine and are useful alone (manual `/join` of any goal session). S4 is the "call
transfer" feel. S5/S5′ harden the storage (S5′ is where the unified-Session model becomes real under
the hood); S6–S7 generalize.

### Remaining order (decided 2026-07-13): ~~S7~~ → ~~S5′~~ → **forking**

S7 was deliberately taken **before** S5′, and forking after both:

- **S7 before S5′** — S5′ is a *schema commitment*, and S7 was the last slice that changed what a
  session *is* (intake rounds, a draft contract, a freeze step). Converging the store before its most
  complex consumer existed would have meant designing the unified `Session` schema blind. It is now
  known.
- **Forking after S5′** — the conversation store is *already a DAG* (`MessageNode.parent_id`,
  `ConversationHeader.{parent_conversation, spawned_by}`, and `leaf_path(conv, Some(leaf))` which
  reconstructs the context prior to any split point). Both features the human wants — branch
  mid-conversation, and fork a session while keeping the original — are **additions to that schema,
  not migrations**. Two things block them: every caller passes `leaf_path(conv, None)` (always the
  newest leaf, so the DAG is a straight line in practice), and `/fork` reaches a **stub**
  (`Effect::ForkConversation` logs and returns; see the `fork_conversation_is_noop` test).
  **Goal sessions cannot branch at all** — their transcript is a *flat* event log with no
  `parent_id`. Giving them node-graph transcripts is exactly S5′, which is why forking waits: the
  valuable version is forking a *coding* session at its freeze point (contract A vs contract B).
- **Fork semantics: COPY, not reference.** A forked conversation copies the prefix nodes rather than
  stitching across `parent_conversation` at read time. This preserves the store's core invariant —
  one conversation is one self-contained, greppable log with its header on line 0 — and gives
  *snapshot* semantics, so continuing the original later cannot mutate the fork. Lineage fields keep
  the relationship visible for the tree view. Forks are rare and transcripts are small; the
  duplication is worth the invariant.

## 6. Open questions (decide during S1/S4, none block starting)

1. ~~**Queue depth for mid-turn input**~~ — settled in S1: bounded buffer (16), input delivered at
   the next await point (the one-writer rule; never interleaved into an in-flight turn). Revisit
   only if a real workload overflows it.
2. **Multiple live interactive sessions** — allowed by the model (focus is per-UI); does the
   session browser need an "awaiting input" badge sort order? (Probably yes, cheap.)
3. **WebUI parity** — after the TUI proof (S3/S4); the wire work carries over unchanged.
4. **Does the face agent see more than the Report?** — recommendation: no; summary + artifacts
   only, per the context-efficiency pillar. The human can always rejoin the transcript.
5. ~~**Naming** — "session profiles" (config key) vs "hats" (docs/UX)~~ — settled in S6 (2026-07-12):
   **"profile"** is the one name, everywhere — `[[session_profiles]]`, `SessionProfile`,
   `/spawn <profile>`. "Hat" survives only as informal prose. Rationale: the same one that retired
   "mesh" — cute internal jargon a newcomer (or a model) cannot infer is a tax, not a feature.

## 7. Docs to touch when implementing

`contracts.md` (`DomainPackRunner` gains the input port + additive-wire note),
`api.md` (endpoint + events), `crates/session/ARCHITECTURE.md`,
`agentic-loops.md` (Surfaces: focus model), `delegate_dogfood_issues.md` (offer/return notes),
and the crate map regenerates untouched (no new crates planned).
