# Session Profiles — Design & Roadmap

**Status**: steps 1–6 landed (the feature is complete end to end) on `feat/session-profiles` (2026-07-28). **Nothing is deployed** — the branch is parked pending the cron-router investigation below.
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
([`channels-and-interactivity.md`](../../spec/architecture/channels-and-interactivity.md)).

Do not rest that claim on "no tool does this" alone — see the loopback exposure in
[`../spec/reference/api.md`](../../spec/reference/api.md#trust-boundary-the-api-is-unauthenticated-and-agent-reachable).
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

### 3. Profile resolution and config shape — **landed** (`d6c1c1a`)

A profile may now declare its own authority instead of pointing at a policy grant. Settled shape,
TOML (the whole config stack is TOML, and these files lean hard on comments):

```toml
[[session_profiles]]
name        = "basic-chat"
description = "Quick answers. Web, search, read-only notes. No dispatch."
delegation  = false
model       = "deepseek/deepseek-v4-flash"
prompt_append = "Answer directly and briefly."
read  = ["Work", "Life"]
write = []
mcps  = [
  "liberado-search-orchestrator-mcp",                    # whole server
  { name = "turbovault", tools = ["read_note"] },        # named tools only
]
```

- **`policy.toml` stays a ceiling, not a bypass.** `ceiling = "<grant>"` narrows the declaration
  against a policy grant, so a profile cannot exceed what the operator allowed there. Optional and
  never defaulted to `name` — a ceiling appearing by accident (no grant of that name) would narrow
  every declaration to nothing, and a profile silently granting no tools is the worst failure here.
- **`write = []` is load-bearing.** `ExecuteTool("turbovault:write_note")` alone does not permit a
  write: the runtime gate checks `Write(<zone>)` separately (since the 2026-07-14 fix). Zones are
  stated, not inferred from the granted tools, whose declarations can change underneath a profile.
- `domain` optional (a chat profile runs the face agent, not a pack); `/spawn` and `POST /api/goals`
  reject a domainless profile by name rather than falling back to a domain nobody picked.
- `resolve_session_profile` returns `ResolvedProfile`, not a 4-tuple.
- Both shapes coexist: `component` (borrow a grant wholesale — the live `research` profile, tested
  unchanged) **or** an inline declaration. Setting both is refused at load.

### 4. Switching — **landed** (`98b830f`, `4359c11`)

- `POST /api/conversations/{id}/profile` (surface-only, never a tool) and
  `GET /api/profiles`.
- Recorded twice: a fresh header line (what the next turn reads) **and** a transcript node authored
  `profile`. `Author::Named`, not `Author::System` — a system node in this store is the face agent's
  prompt and every reader drops those, so a `System` note would have been invisible in the WebUI.
- Applies on the **next** turn; the response says so, since the runtime is rebuilt per turn.
- Typed `delegation` / `model` / `prompt_append` on `SessionGrant` rather than `overrides`, which is
  opaque by contract and parsed only by a pack — and a chat has no pack, so anything there would be
  read by nobody. `ResolvedProfile::grant_parts` keeps the mapping in one place.
- `/profile` on the shared `Picker` (its third caller), an active-profile chip in the chat, and
  `ConversationHistoryResponse.profile` so the chip is right from first paint rather than only after
  a switch.
- Refusals tested: unknown profile → 400 with the grant untouched (a typo must never resolve to "no
  profile", which silently means the wider default); unknown conversation → 404; clearing allowed.

### 5. Per-session delegation + prompt nudge — **landed**

- `delegation` resolved per turn from the session's profile. `None` means *inherit*, not off — only
  an explicit `delegation = false` disables dispatch.
- `prompt_append` injected as a **second** system message. The first is the conversation's persisted
  root node and the store is append-only, so editing it would mean rewriting history on every
  profile change. It sits with the system prompt, not among the dialogue.
- One `turn_settings` lookup resolves all three, so a switch cannot land *between* them and produce a
  turn running one profile's tools under another's delegation setting.
- **`model` — landed.** Was deferred: the model lives on the provider, not the request, so
  per-session selection needed a per-call field or a provider pool. Took the first, because for an
  OpenAI-compatible backend the model is simply a body field. `CompletionRequest.model` wins over
  the provider's own, including one hot-swapped via the TUI's `/model` — naming a model in a profile
  is an explicit per-session choice, a swap is a daemon-wide default. `Executor` is `Clone` over two
  cheap fields, so `ChatSessions` specialises one per turn via `with_model` rather than mutating a
  provider every other session shares. `TurnSettings` carries it with the rest, so a switch cannot
  land between two reads and give a turn one profile's model under another's tools.

### 6. Surfaces — **landed**

Two gaps found by looking at the running UI, both invisible in the code:

- **The first turn always ran on the default grant.** A profile could not be chosen before the
  conversation it applies to existed. `ChatRequest` now carries `profile` beside `incognito`, same
  rule: consulted only when creating.
- **The chip only appeared once a profile was set**, so a new chat showed no control at all. Now
  always rendered, reading "default" when unset.

The Status screen got a **read-only** list, not a switcher: everything else there is daemon-scoped,
so a switcher would have to ask which conversation you meant — the chat's question.

### 7. Derived system prompts — **not started**

A profile already owns both halves of the thing that drifts: a tool set (`mcps`) and prompt text
(`prompt_append`). Today they are written independently, so a profile can instruct the model to use
a tool it was never granted, and nothing anywhere notices. That is the VTCode failure mode exactly —
their `prompts.coder` named `write_file`/`list_dir`/`read_file` when the shipped toolset exposed
`unified_file`/`unified_search`, and their system prompt told the model to use `task_tracker`, which
was not among the 17 tools it was offered. Two sources of truth about what exists, no validation
between them, and the symptom (a model that explores and never acts) looks nothing like the cause.

**Single-source the value; do not merely keep two copies agreeing.** The weak version of this fix
generates prompt prose from config, so the two are consistent at load time. The strong version
renders the prompt's tool section from *the same* `Vec<ToolDef>` handed to the provider — one value,
two renderings — so disagreement is unrepresentable rather than prevented. Prefer the strong version
wherever a prompt states a fact the runtime already holds.

**Split by provenance, not by config key.** Two kinds of text live in a system prompt and they want
opposite treatment:

- *Facts about this session* — which tools exist, whether the session may ask a human, timezone,
  vault root. The resolved profile already knows these; hand-writing them duplicates a source of
  truth. **Derive.**
- *Voice, priority, judgment* — the non-negotiable role framing in `HUMAN_INTERFACE_SYSTEM_PROMPT`,
  "be concise", "do not skip clarifying questions". Not derivable from any config, and encoding it
  in TOML would be strictly worse than prose in a Rust const. **Keep authored.**

Append the derived part as a delimited block; do not weave it into tuned paragraphs. VTCode is
evidence that prompt *structure* is load-bearing: their `system_prompt_mode = "minimal"` experiment
made behaviour worse, not better (14 turns vs. the usual 4-6, the same file re-read seven times in
overlapping chunks), which says a mechanically thinned prompt can cost more than the tokens it saves.

**The capability statement is the largest win.** A session's prompt never says what the session
*cannot* do. A cron holds no `AskHuman`, and nothing tells the model that. Derived, it would state
plainly: *you cannot ask the human anything; if you lack information, report what is missing and
stop.* Note honestly what this does and does not buy — it would **not** have prevented the 2026-07-28
cron failure below, which degraded to `Clarify` from a JSON parse failure with no model decision
involved. It closes the adjacent case, where an unattended model legitimately chooses `Clarify`
because nothing told it clarification was impossible.

Mechanism fits what step 5 already established: the derived block is composed **per turn** and
injected as a system message alongside `prompt_append`, never written into the conversation's root
node — the store is append-only and the root is persisted, so a session whose tool set changes must
not rewrite history to say so. Resolve it in the same `turn_settings` lookup, for the same reason
that lookup exists: so a switch cannot land between two reads.

Precedent already in the tree: the daemon prepends `Local time: … (America/Chicago)` to cron goals.
That is a derived prompt fact, just ad hoc and in one place.

**Build the inspector first.** Once a prompt is composed rather than written, it can be printed:

```
liberado prompt --profile basic-chat      # exactly what the model will see
```

This is the diagnostic VTCode needed and did not have — "success but no writes" survived eight
hypotheses partly because nobody could cheaply compare what the model was *told* against what it was
*offered*. A hand-written const cannot offer this once appends and overrides land on top of it.

**Bound the variation, or this becomes the disease it cures.** Every conditional is a variation
point; four booleans is sixteen possible prompts, none of which a human ever reads. Keep the derived
block mechanical — lists and flat statements, no branching rhetoric. Anything needing an `if` inside
a sentence belongs in the authored half. One snapshot test per shipped profile, asserting both
directions (the `delegate` tool appears iff `delegation`, a tool absent from `mcps` appears nowhere
in the text).

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

## Blocked on: cron router failures (2026-07-28)

Both `daily-planning` and `evening-debrief` failed that morning. The Telegram message leads with
capability language, but **it is not a permissions problem**:

```
classify{model=deepseek/deepseek-v4-flash}: structured output did not decode — retrying once
  error=failed to decode structured output: expected value at line 1 column 503
        — finish_reason=Stop, completion_tokens=267, reply was 964 chars
→ classification produced unusable output; degrading to Clarify
→ dispatch decision downgraded by guard  downgrade=Unattended
```

The **router model returned unparseable JSON**, twice (the retry failed too), and `finish_reason=Stop`
rules out truncation. The dispatcher correctly degraded to `Clarify`; the Unattended guard then
blocked it because a cron holds no `AskHuman`. The capability advice in the message is the
*consequence*, and acting on it — granting `AskHuman` — would be exactly wrong: it would leave an
unattended 06:55 cron waiting on a person instead of failing.

Worth fixing the message itself, which sent the operator down that path.

## Live test, 2026-07-28 — profiles work; the prompt does not follow them

Deployed `0aac75f` to the homelab, added a `basic-chat` profile to the live `topology.toml`, and
drove the WebUI headlessly. **The capability half passed completely.** Three sessions in one daemon,
three authorities:

| session | profile | caps | writes | tools |
|---|---|---|---|---|
| chat B (control) | `None` | 0 | none | daemon default |
| chat A | `basic-chat` | 5 | **none** | search whole + 2 named turbovault tools |
| cron | `None` | 36 | 10 zones | 6+ whole MCPs |

`write = []` produced no `Write` capability at all; the narrowed entry produced two `ExecuteTool`
grants rather than a whole-server `ExecuteMcp`; empty-grant and no-profile stayed distinguishable.
The chip, the picker, and profile-before-first-message all worked end to end.

**And the session did nothing.** Asked "What tasks do I have open?", the model replied *"I'll fetch
your open tasks first."* and issued **zero tool calls** — announced an action, took none. The exact
symptom [`pr-dispatch-vtcode-no-write-finding.md`](../pr-dispatch-vtcode-no-write-finding.md) spent
three rounds on, reproduced in our own system on the first live run.

The cause is the drift step 7 predicts, now concrete:

- [`crates/server/src/lib.rs`](../../../crates/server/src/lib.rs) ~750 picks the system prompt **once at
  daemon construction** from the global `main_agent.delegation_mode`.
- [`crates/main-agent/src/sessions.rs`](../../../crates/main-agent/src/sessions.rs) ~479 persists that
  one prompt as every conversation's root node.
- The same file ~580/~640 resolves `uses_face_agent(settings.delegation)` **per turn, per profile**.

So step 5 made the tool surface per-session and left the prompt daemon-wide. A `basic-chat` session
is handed `HUMAN_INTERFACE_SYSTEM_PROMPT` — *"You are a face agent, not a tool user… call the
`delegate` tool"*, plus *"Do not try to enumerate tools from your own context. You will usually see
only `delegate`"* — while holding no `delegate` and five other tools. The model obeyed the prompt
over the tool list, which is the correct thing for it to do and the wrong thing for us to have asked.

**This makes step 7 a blocker, not a nicety.** `basic-chat` cannot ship until the prompt follows the
profile. Note the fix cannot be "choose a better prompt at conversation creation": a profile is
switchable mid-conversation and the root node is persisted append-only, so creation-time selection
would be wrong on the very next switch. It has to be composed per turn beside `prompt_append` —
which is what step 7 already specifies, for exactly this reason.

Minimum viable slice, smaller than full step 7: when `uses_face_agent(delegation)` is false, swap the
model-visible system message for one that describes the tools the session actually holds. The derived
`## Environment` block and the `liberado prompt` inspector can follow.

Two smaller findings from the same run:

- **The per-turn tool surface is never logged.** `chat: tool surface ready` fires once at boot with
  the daemon default; there is no way to see what a given session was offered. Diagnosing the above
  required reading the stored grant over the API and then the source. Worth a per-turn log line.
- **`wasm-opt` crashes** (`0xc0000409`) during `deploy-webui-homelab.ps1`; the build falls through and
  ships an unoptimized bundle. Works, larger than it should be.

## Before deploying

**Do not add a chat profile to the live `topology.toml` until this branch is deployed.** On the
currently-deployed build `SessionProfile.domain` is a required `String`; it only became optional in
step 3. A chat profile omitting it fails to deserialize, and config errors are fail-fast — the daemon
would refuse to boot.

~~Still to do: a live test putting a real chat into `basic-chat`.~~ **Done 2026-07-28** — see the
live-test section above. The tool surface shrinks correctly; the system prompt does not follow it,
which blocks `basic-chat` on step 7.

## Open questions

- Where a chat's *resolved* non-capability settings (model, prompt_append, delegation) ride from
  config to the running turn. `SessionGrant.overrides` is documented as opaque and pack-parsed, so
  typed fields on `SessionGrant` are the likely answer — decided in step 4/5.
- `may_delegate_to` — deferred, shape sketched above.

### ~~Stale tool exchanges survive a profile switch~~ — measured 2026-08-01, closed

**The manifest is sufficient. Do not build the history rewrite.**

Measured rather than reasoned about. A chat under `basic-chat` called `turbovault:tasks_list` and
got real data; the profile was switched to a `search-only` profile with no vault access; the next
turn asked for something the earlier result could not answer — *"re-check my open tasks right now and
tell me whether anything changed"* — so answering from memory was unavailable and the model had to
decide whether it still held the tool. Daemon logs confirm the setup: `count=2` then `count=0`.

It answered: *"I can't re-check right now — I don't have access to tools on this turn. **The last
result I saw showed** 8 open tasks…"* It refused, and marked the stale data as stale unprompted.
A current, complete list stated in the system position beats a successful tool call sitting in the
transcript.

So option 3 (rewrite historical exchanges) is unnecessary, and with it the cache cost that was the
main objection. Option 1 (do nothing) was never quite right either — the manifest *is* the fix, it
just already exists.

One defect the measurement did surface, since fixed: the empty manifest said "no tools **on this
turn**", and the model deferred — *"ask me again on the next turn and I'll do a fresh lookup"* —
which was untrue, the profile lacking the tool entirely. Accurate about the turn, misleading about
the future, and the same announce-then-cannot shape as the bug the manifest was written for. It now
forbids suggesting a retry, while explicitly permitting honest citation of earlier results as
earlier — which the model had already got right on its own.

### Original analysis

Raised by the operator 2026-07-28, after the prompt fix: a conversation that used profile A's tools
and then switches to profile B still carries A's `tool_calls` and tool results verbatim. Rehydration
is `nodes.iter().map(|n| n.message.clone())` with no capability filtering, so a model that watched
itself succeed at `spider:fetch` has in-context evidence that outlives the grant.

**This is a coherence problem, not an authority one**, and the distinction should survive into
whatever fixes it. Tool calls are constrained to the declared catalog, so a stale memory cannot
become a stale invocation, and `ScopedRuntime` fails closed per-tool if one somehow arrived. Nothing
is reachable that should not be. The expected symptom is the model *claiming* a capability or
offering to re-run a lookup it can no longer perform — the same announce-then-stall shape as the
prompt bug, arriving by a different route.

The existing mitigation is weak: `set_profile` appends `Message::system("Session profile: basic-chat")`
and that is rehydrated, but it names a *profile*, not a capability set. The model cannot know what
`basic-chat` grants, so it reads as a topic change rather than a tool revocation, and loses against a
concrete successful call three turns up.

**Naive filtering is not available.** Providers validate that an assistant message carrying
`tool_calls` is followed by matching tool results; drop the result and the orphaned call 400s, drop
both and the turn's causal record is corrupt. Any real version *rewrites* the exchange into a neutral
note rather than removing it.

**On the cache cost** (the operator's objection to stripping): rewriting history does invalidate the
cached prefix from the rewrite point, and the earliest stale call is usually early. But the cost is
**bounded and one-off, not per turn** — the filtered view is a pure function of the current grant, so
once switched it is stable and re-cacheable until the next switch. One cold turn per switch, not a
permanently cold conversation. That is affordable; it is the *complexity* of rewriting exchanges, not
the cache, that argues against doing it.

Three options, in increasing cost:

1. **Nothing.** Rely on catalog gating. Accept occasional incoherent claims.
2. **State current capability per turn** — step 7's derived block. Beats stale evidence on position
   (system, not buried mid-thread) and recency (this turn). Costs nothing extra once step 7 exists,
   since `turn_settings` already resolves the capability set. **Preferred.**
3. **Rewrite historical exchanges** for tools no longer granted. Most thorough, most machinery, and
   the cache cost above.

An interim cheaper than (2) is making the switch note say what changed rather than naming a profile
— "Session profile: basic-chat. Tools now available: …". One line, no new machinery, but it
*persists a claim* that goes stale if the profile's config is later edited: the drift argument for
deriving per turn instead of writing down.

**The operator's broader read, worth keeping:** that switching profiles while retaining context may
be the wrong default. Fork-then-switch sidesteps all of this — a fresh conversation has no stale
exchanges to disagree with — and is already available, since profile switching deliberately does not
offer to fork ([Build order](#build-order) step 4). If (2) proves insufficient in practice, the
answer is more likely "recommend forking" than "build (3)".
