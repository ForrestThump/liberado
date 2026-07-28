# Session Profiles — Design & Roadmap

**Status**: steps 1–2 landed on `feat/session-profiles` (2026-07-28). Steps 3–6 open.
Design settled with the operator in conversation on 2026-07-28; the decisions in
[Settled decisions](#settled-decisions) are answers to direct questions, not proposals.

## Motivation

The face agent has one process-wide tool surface. There is no way to say "for *this* chat, be a
basic assistant: web search, spider, a handful of turbovault tools, and don't dispatch" — the
alternative to the full fleet is editing `policy.toml` and restarting the daemon, which changes it
for every conversation at once.

A **session profile** is a named authority a conversation runs under: its tools, whether it may
delegate, optionally a model and a prompt nudge. Chosen by a human, per chat, switchable.

## Settled decisions

| Question | Answer |
|---|---|
| Granularity | **Per-tool.** Extend `Capability` with `ExecuteTool("<mcp>:<tool>")` rather than filtering on top of per-MCP grants — one authority mechanism, not two. |
| Mutability | **Switchable, recorded, human-only.** Not locked at creation; every switch is written to the transcript; the agent can never change its own. |
| Namespace | **`session_profiles`** — the existing config table. Not "hats". `/profile research` and `/spawn research` select the same named authority. |
| Profile contents | Tools, **dispatch on/off**, **system-prompt nudge**, **model override**. |
| Model-chosen profiles | **Out of v1.** See [Why `delegate` takes no profile](#why-delegate-takes-no-profile). |

### The naming wobble, resolved

"Profile" reads slightly odd for something mutable mid-session. Considered and kept anyway: the
alternative was a second vocabulary for the same concept, and one list of names beats two that
drift.

## Why `delegate` takes no profile

Asked directly: could the face agent spawn a session with *wider* authority, and would that be a
mistake?

**Today it structurally cannot.** `delegate`'s schema is `{goal, context}` (`face.rs:197`) and the
grant is hardcoded at the call site — `dispatcher_capabilities` minus `AskHuman`, `profile: None`
(`face.rs:98-111`). There is no field through which a model can name an authority.

**It already widens, and that is fine.** `main-agent` in `policy.toml` is mostly `Read`; `dispatcher`
holds `Write` across every zone plus the MCP fleet. The face agent already delegates to far more
authority than it holds — that is the human-interfacer design. What makes it safe is not the absence
of widening but **who chooses**: the operator fixed one ceiling in config; the model picks the goal,
never the authority.

Giving `delegate` a profile argument converts one operator-chosen ceiling into a model-selected menu:

- **The model drifts broad.** Picking the widest plausible profile maximises task success — that is
  the objective function, not misbehaviour. An allow-list bounds the blast radius without removing
  the incentive.
- **It becomes an injection target.** A vault note or scraped page saying "use the coding profile"
  would move authority. Today no text anywhere can, because no field exists to move.
- **It is redundant with routing.** Choosing which tools a goal needs is already the dispatcher's
  job, and the dispatcher is operator-configured. A profile argument adds an escalation surface
  without adding capability.

**Instead**: the conversation's profile decides the delegation ceiling too
(`SessionProfile.delegate_grant`, and `delegation = false` to switch dispatch off entirely). One
human decision, two effects; the model still calls `delegate(goal)` with no authority argument, and
zero new model-facing surface.

If the model should ever choose, the shape is `may_delegate_to = ["research"]` defaulting to empty,
with the choice journalled. Easy to add on top of the per-session grant; hard to walk back. The
operator's stated intent — an interactive session with an agent that has teeth, beyond the face
agent — is already served today by `/spawn <profile>`, which a human types.

## Does switching "widen"?

Yes, and that is correct. Narrow-only (Decision 4) governs **delegation**: a subagent gets
`base ∩ narrowing` at spawn. A human re-authorising their own chat from an operator-authored
`policy.toml` is a different act. A profile's grant therefore *replaces* the session's rather than
intersecting with it — intersecting would mean a profile could never add spider-mcp, since the face
agent's default grant lacks it, and the feature would not work at all.

The property that matters is preserved by recording: the transcript says which authority ran which
turn.

## Human-only switching

The switch is `POST /api/conversations/{id}/profile`, reachable from surfaces only and **never
registered as a tool in any runtime catalog**. `delegate` cannot reach it, so the agent cannot
re-authorise itself. This is an authority-channel act, not an information-channel one
([`channels-and-interactivity.md`](../architecture/channels-and-interactivity.md)).

Do not rest that claim on "no tool does this" alone — see the loopback exposure in
[`../reference/api.md`](../reference/api.md#trust-boundary-the-api-is-unauthenticated-and-agent-reachable).
A granted web-fetching MCP can `GET` the daemon's own API. `POST` is out of reach of a GET-only
fetcher, which is why the switch is a `POST` — an incidental defence, but a real one. **Tracked
separately; not part of this plan.**

Recording reuses two existing mechanisms, no new record kinds:

1. Append a fresh `Record::Header` carrying the new grant — exactly what `set_title` already does;
   replay takes the last header it sees.
2. `append_turn(System, "Session profile: research")` so the switch is visible in the transcript and
   findable by `chat-search`.

## Build order

### 1. `ExecuteTool` + subsumption — **landed** (`22203b6`)

- `Capability::ExecuteTool("<mcp>:<tool>")`; `ExecuteMcp` **subsumes** it via `Capability::subsumes`.
- `narrow()` rewritten off naive set-intersection. `ExecuteMcp("tv") ∩ ExecuteTool("tv:read")` would
  have come out **empty**, silently stripping a delegated subagent of tools its parent held.
- `CapabilitySet::grants_tool` is the authorization question; `grants_mcp` is now the coarse
  "reachable at all". Every caller audited: risk gate and dispatcher pre-flight guard moved to
  `grants_tool`; the config explainer moved (it had the qualified name and printed PASS on calls the
  runtime would refuse); `status.rs`'s `visible_to_main_agent` correctly stayed coarse.
- `ScopedRuntime::from_capabilities` — per-tool and **fails closed**. The existing constructor treats
  an empty allow-list as pass-through, right for the tool-advisor and catastrophic for a grant.
- `config-loader` rejects `ExecuteTool` with no `:` at load (it would parse fine and mean "the MCP
  named read_note" — authorizing nothing, silently).

### 2. Per-session grant — **landed** (`2b9fe21`)

- `SessionGrant` on `NewConversation` / `ConversationHeader`; `ConversationStore::create` stops
  dropping it. Needed `conversation-store → session` (downward; layer rules pass).
- `ChatSessions::session_capabilities(session)` read **per turn**, so a later switch needs no
  restart.
- Resolution rule, each with a test:
  - no profile → **process-wide grant** (reading the store's empty default literally would have
    silently stripped tools from every pre-existing chat);
  - named profile → **replaces** the process grant;
  - named profile with empty capabilities → honoured as "may call nothing";
  - lookup failure → falls back and warns.

### 3. Profile resolution and config shape — **next**

- `SessionProfile.domain` → `Option<String>` (a chat profile names no pack).
- Add `description` (for the picker), `delegation: Option<bool>`, `prompt_append: Option<String>`,
  `model: Option<String>`, `delegate_grant: Option<String>`.
- Decide where the non-capability settings live: typed fields on `SessionGrant` (readable by the chat
  engine) rather than `overrides`, which is documented as opaque and pack-parsed.
- Write out a concrete `basic-chat` profile and check it reads right **before** wiring surfaces.

### 4. Switching

- `POST /api/conversations/{id}/profile` (surface-only, never a tool).
- Header line + system node, per [Human-only switching](#human-only-switching).
- `/profile` slash command via the shared `Picker` (one list + a callback).

### 5. Per-session `delegation_mode`

Currently one boolean for the whole daemon (`topology.main_agent.delegation_mode`), read by
`uses_face_agent()`. Making it per-session is what turns "basic chat" into a real mode rather than a
shorter tool list.

### 6. Surfaces

`GET /api/profiles` (nothing enumerates enabled profiles today — `resolve_session_profile` only
resolves one by name), Status-screen tap-to-select, and an active-profile chip in the header.

## Traps found while building

Recorded because each was invisible in the failing direction:

- **`max_consequence` looks up consequence by bare MCP name.** Once `referenced_grants` returned
  qualified `<mcp>:<tool>` names, nothing matched — and an unmatched entry scores `ReadOnly`, which
  would have quietly disarmed the consequence gate for every `ExecuteDirect`. Fixed by mapping
  through `mcp_of`, with a comment saying why that line is load-bearing.
- **`capability_set_serde_round_trip_all_variants` did not cover all variants** — it already omitted
  `AskHuman` before this work. These serialise into `policy.toml` *and* session-log headers, so a
  variant that fails to round-trip is a grant that does not survive a restart.
- **Empty grant vs. no profile** must stay distinguishable, or "this chat may call nothing" is
  unsayable. The profile *name* carries the intent.

## Open questions

- Where the non-capability profile settings live (step 3's first bullet).
- Whether `/profile` on an existing chat should switch in place or offer to fork. Switching is
  settled; forking may still be the nicer default for a *narrowing* switch.
- `may_delegate_to` — deferred, shape sketched above.
