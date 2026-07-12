# Session focus & interactive goal sessions — plan

**Status**: plan, 2026-07-12 — no code yet.  
**Decision already made**: the "call transfer" to a specialist is **not a new process or daemon**.
It is the UI moving its *input focus* onto an interactive goal session running on the same hub,
same daemon, same wire contract. Transcripts are **separate but linked** (the same pattern
delegation already uses: dispatch journals + correlation ids).  
**Related**: [`agentic-loops.md`](../architecture/agentic-loops.md) ·
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

### G3 — The offer: how a chat turn hands the human a session

- **Dispatch side**: the dispatcher (or the face agent's `delegate` result) marks a decision as
  *interactive* — a goal whose success needs a human in the loop (development work, anything
  opening with intake). Likely a field on the decision/goal rather than a new action type.
- **Server side**: when a chat turn spawns an interactive session, the chat stream emits
  `SessionOffered { session_id, domain, description }` before `session_finished`. Explicit event,
  not client-side sniffing of tool-result text.
- **Linkage**: the spawned session's `origin` carries the conversation id + correlation id; the
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

- `GoalSessionStore` is **in-memory only** — a restart loses transcripts. Add an append-only JSONL
  transcript log under `<LIBERADO_DATA_DIR>/goal-sessions/` (same Decision-12/17 reasoning:
  operational data lives outside the vault; conversation-store is the pattern). Rehydrate the
  list/snapshot views from it on boot.
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
| S3 | TUI focus MVP: `/join <id>` + `/back` + session-kind renderers (no offer yet) | Live smoke: full Q&A with the life demo through the TUI |
| S4 | The offer + return handoff: interactive flag on dispatch, `SessionOffered`, summary folded into parent conversation, journal cross-links | Live smoke: "build a hello CLI" → offer → join → watch → `/back` → summary in main chat |
| S5 | Durable session transcripts (JSONL) + rehydrate on boot | Restart daemon, rejoin a finished session from the browser |
| S6 | Hat profiles in topology + "new session as <hat>" | Config-driven second hat (e.g. `research` on the life pack) with a narrower component grant |
| S7 | Intake-first coding sessions (clarify → freeze UI in the TUI) | The §3.4 worked example (todo CLI) end-to-end from chat |

S1–S3 are the spine and are useful alone (manual `/join` of any goal session). S4 is the "call
transfer" feel. S5–S7 harden and generalize.

## 6. Open questions (decide during S1/S4, none block starting)

1. ~~**Queue depth for mid-turn input**~~ — settled in S1: bounded buffer (16), input delivered at
   the next await point (the one-writer rule; never interleaved into an in-flight turn). Revisit
   only if a real workload overflows it.
2. **Multiple live interactive sessions** — allowed by the model (focus is per-UI); does the
   session browser need an "awaiting input" badge sort order? (Probably yes, cheap.)
3. **WebUI parity** — after the TUI proof (S3/S4); the wire work carries over unchanged.
4. **Does the face agent see more than the Report?** — recommendation: no; summary + artifacts
   only, per the context-efficiency pillar. The human can always rejoin the transcript.
5. **Naming** — "session profiles" (config key) vs "hats" (docs/UX). Pick one before S6.

## 7. Docs to touch when implementing

`contracts.md` (`DomainPackRunner` gains the input port + additive-wire note),
`api.md` (endpoint + events), `crates/session/ARCHITECTURE.md`,
`agentic-loops.md` (Surfaces: focus model), `delegate_dogfood_issues.md` (offer/return notes),
and the crate map regenerates untouched (no new crates planned).
