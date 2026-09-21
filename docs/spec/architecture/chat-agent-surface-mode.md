---
kind: plan
status: active
authority: implementation
domain: product
canonical_for: chat-agent-surface-mode
open_items: true
---

# Chat vs agent surface mode

**Status**: Slice 1 (wire + stamp) landed in [#274](https://github.com/ForrestThump/liberado/pull/274). Slice 2 (WebUI shelves + create-path + privileged `create_agent`) landed in [#276](https://github.com/ForrestThump/liberado/pull/276). The CAS1 follow-ups — making `agent_profiles` deployment-tunable via `[chat]` in `tuning.toml` and ensuring every create / read path honours the deployment's set instead of the conservative default — landed in [#277](https://github.com/ForrestThump/liberado/pull/277). **Next is CAS3 (chat-default tools, Slice 3)** — see [`docs/roadmap.md`](../../roadmap.md) and [`docs/future-work/backlog.md`](../../future-work/backlog.md). Jev is out of scope for this document (CAS1–CAS4 only); the full phase plan lives in [`jev-integration.md`](jev-integration.md) and starts only after the shelves are dogfooded.

**Reading**: this plan locks **Reading B** for the meaning of "agent". An
**agent** is a long-lived specialist **chat** (a Grok-Bot-style context with
curated tools, never terminal). It is **not** `goal.is_some()`. The stamp is
chosen from create-time signals — explicit surface mode, or the named profile
falling in a configured `agent_profiles` set — and **not** derived from
`SessionHeader.goal`. The reasoning is in §1.

## 1. What "agent" means in this plan

The brief gave two readings:

- **Reading A**: agent ≡ `goal.is_some()`. A `/spawn`ed coding goal lands in
  Agents; an open-ended chat stays in Chats. The split is a free projection
  off `goal.is_some()`.
- **Reading B**: agent ≡ a long-lived specialist **chat** — a curated-hat
  context, Grok-Bot style, never terminal. `/spawn` may produce one, but so
  may a chat opened under the `coding` profile.

This plan locks **Reading B**. The reasons are concrete:

1. **The product sense.** "Agent (Grok-Bot feel: long-lived specialist
   contexts)" describes a surface the human keeps open and talks to over
   days, not a `/goal` that runs to completion and parks. `goal.is_some()`
   is the wrong attribute: a chat that picks the `coding` profile is the
   thing the brief names, and it is goal-less by construction.
2. **The kernel stays unchanged.** Capability `∩`, dispatcher sole
   grantor, `SessionGrant` resolved once, no peer mesh — those rules are
   not touched. The shelf split is a **client-side** distinction; the
   kernel does not branch on it.
3. **The stamp is operator-visible.** `agent_profiles` is a small,
   named, configurable set (default: `coding`, `life`, `researcher`; see
   §3). Adding a new specialist hat means adding a name, not a kernel
   change. Jev's eventual "which agent does this belong to" wedge reads
   more naturally against an addressable set of named profiles than
   against a projection of `goal.is_some()`.

Reading A is rejected. The consult's "free projection" recommendation
sounded cheap, but it would shelf every `/spawn` and miss the long-lived
chat-under-a-profile case the brief is naming.

## 2. Where the stamp lives (and where it does not)

| Layer | Field | Why here |
|---|---|---|
| `chat-client-contract::ConvHeader` | `surface_mode: SurfaceMode` (wire) | Every chat client renders this. `#[serde(default)]` to `"chat"` so old clients ignore it. |
| `conversation-store::ConversationHeader` | `surface_mode: SurfaceMode` | Persisted on disk; mirrors the wire. Default `Chat` for legacy rows. |
| `session-store::SessionHeader` | `surface_mode: SurfaceMode` | The kernel-side record; needed because a chat's projection flows through `SessionHeader`. |
| `NewConversation` / `NewSession` | `surface_mode: SurfaceMode` | The create-time signal. `Default = Chat` so every existing call site stays chat unless it opts in. |
| `SurfaceMode` enum | `chat-client-contract::wire::SurfaceMode` and `conversation-store::SurfaceMode` (parallel) | Two small parallel enums with identical serde representations. Stores cannot depend on the client crate (layer rules); the duplication is intentional and small. |
| `SessionGrant` | — **not extended** | The grant is the authority ceiling; a `kind` field there would invite kernel-side branching. The shelf split is not authority. |
| `SessionKind` chip enum | — **not extended** | `SessionKind::Primary.tag() == "CHAT"` is a chip label for the TUI's pack-domain view, not a product sense. Adding a 5th variant called `Chat` would collide with `Primary` and the product term orthogonally. |

The stamp is **create-time + profile-class**, **never** derived from
`goal.is_some()`. A goal session with `goal = Some(_)` and no profile
still defaults to `Chat` on the wire — because the goal session is not
the surface the brief names, and Reading B says the agent sense is the
profile one.

## 3. How `create` picks chat vs agent

The stamp is decided in **one** place: `ChatSessions::create_conversation`
in `crates/main-agent/src/sessions.rs`. Every create path routes through
it (`create`, `create_with_grant`, `create_background`, `create_incognito`,
plus the kernel's `SessionStore::create_session` for goal sessions, and
the chat lens's `to_conversation_header` for legacy rows).

The decision:

```
stamp_surface_mode(grant: &SessionGrant, explicit: Option<SurfaceMode>) -> SurfaceMode
    if let Some(m) = explicit: return m
    if let Some(profile) = grant.profile.as_deref():
        if profile is in agent_profiles: return Agent
    return Chat
```

`agent_profiles` is a small **deployment-tunable** set. The default —
the conservative CAS1 set — is `coding`, `life`, `researcher`, `operator`,
pinned as `AgentProfiles::DEFAULT_NAMES` in
`crates/conversation-store/src/types.rs`. A deployment that adds a new
specialist hat sets `[chat] agent_profiles = [...]` in `tuning.toml`;
the daemon reads it at boot and threads the same `AgentProfiles` into
both `ChatSessions` (create-time stamp) and `SessionStore` (chat-lens
projection on every read). Removing a default name is a deploy-visible
change — legacy rows whose profile is no longer in the set project back
as `Chat` after the next daemon restart. See §6c for the wiring
and [`tuning.md`](../reference/tuning.md) for the knob.

The free function `is_agent_profile(name)` is kept as a backward-compat
shortcut for `AgentProfiles::default().is_agent(name)`; it is **not**
the source of truth. Production read paths (`stamp_surface_mode`,
`to_conversation_header_with`, `/api/profiles`, the face
`create_agent` tool) all read from the deployment-tuned set on
`ChatSessions` / `SessionStore`, never from the free function.

The list is **conservative**: every name here is a profile that exists
in the homelab's `policy.toml` and that the operator treats as a
specialist chat hat. Adding a new name is a one-line change; the
operator can also override the stamp explicitly on any create that has
an explicit signal (slice 2 work).

**Legacy compat.** A pre-stamp on-disk `SessionHeader` has no
`surface_mode` field; serde defaults it to `Chat`. The chat lens's
`to_conversation_header` **upgrades** legacy rows: if
`surface_mode == Chat` (the default) **and** `grant.profile` is in
`agent_profiles`, it stamps `Agent` for the projection only — the
on-disk header stays `Chat`. This way the user sees a chat that was
opened under the `coding` profile in 2026-09 in the right shelf
without any migration.

## 4. Slice order

| Slice | Scope | PR size | Status |
|---|---|---|---|
| **S1 — wire stamp** | Add `SurfaceMode`, stamp at create paths, persist on headers, expose on `ConvHeader`. Tests for serde roundtrip and stamp logic. | one | **Done** ([#274](https://github.com/ForrestThump/liberado/pull/274)) |
| **S2 — WebUI shelves + create-path** | Two top-level shelves inside the sidebar (Chats / Agents), partition client-side by `ConvHeader.surface_mode`. Default to Chats. **New Agent** create-with-grant. Privileged face `create_agent` tool (gate A). | one | **Done** ([#276](https://github.com/ForrestThump/liberado/pull/276)) |
| **CAS1 follow-ups** | `AgentProfiles` deployment-tunable via `[chat]` in `tuning.toml`; cross-PR consistency so every create / read / pick path consults the deployment's set, not the conservative default. | one | **Done** ([#277](https://github.com/ForrestThump/liberado/pull/277)) |
| **S3 — chat-default tools** | Named `chat-default` `[[session_profiles]]` entry; `chat-search` (read-only MCP) granted to `main-agent`. Operator opt-in via `policy.toml` comment block. | one | **Next.** Backlog: CAS3. |
| **S4 — dogfood + measure** | One-week dogfood of shelves + chat-default. Tune `agent_profiles` set from observed usage. | one | Blocked on S3. Backlog: CAS4. |
| **later — Jev** | Out of scope for this plan. See [`jev-integration.md`](jev-integration.md) for the phase plan (first wedge, non-goals, kernel constraints). Lands after S4 has measured the shelves for a week. | n/a | Blocked on S4 dogfood.

TUI is **deliberately not in this slice set**. The TUI already renders
its own `SessionKind` chip from `goal.domain`; a kind filter is parity
work, not product work, and Forrest does not use the TUI daily. Mark
the TUI maturity plan as "shelf split deferred" so the next agent does
not pull TUI filter work into this PR.

## 5. Non-goals (Slice 1)

Per the brief and confirmed by walking the repo:

- **Jev** (brief line 12). Out of scope here; see [`jev-integration.md`](jev-integration.md).
- **TUI polish / TUI shelf split.** TUI already has the kind chip; no
  shelf work for Slice 1.
- **Dispatcher / executor refactor.** Capability `∩` and dispatcher
  sole grantor are not touched.
- **Peer agent mesh / A2A.** Already rejected by
  `docs/future-work/research/agent_pools_research_results.md`.
- **New `SessionKind` variants.** Four-variant enum has three
  downstream consumers; do not grow it.
- **New `SessionGrant` field.** Forbidden — see §2.
- **New MCP server / new policy.toml change.** Slice 3+ work.
- **Full WebUI shelf redesign.** Slice 2.

## 6. Slice 1 acceptance criteria

**Status**: all criteria below met in [#274](https://github.com/ForrestThump/liberado/pull/274).

### Functional

1. `GET /api/conversations` returns rows with `surface_mode` equal to
   `"agent"` when stamped, `"chat"` otherwise.
2. A legacy on-disk row (no `surface_mode` field) deserializes as
   `Chat`, and the chat lens's `to_conversation_header` upgrades it to
   `Agent` **iff** the row's `grant.profile` is in `agent_profiles`.
3. `POST /api/chat/stream` with no `profile` opens a `Chat`. With a
   profile in `agent_profiles` it opens an `Agent`. The `profile`
   query/body field still resolves to a `SessionGrant` (existing
   contract preserved).
4. A `POST /api/goals` goal session defaults to `Chat` (no profile,
   no explicit stamp). Reading B: the goal session is not the surface
   the brief names.
5. An old client reading a new daemon ignores `surface_mode`
   (`#[serde(default)]`).
6. A new client reading an old daemon sees rows with no
   `surface_mode` and treats them as `"chat"`.

### Tests

7. `chat-client-contract::ConvHeader` round-trip: missing field
   defaults to `Chat`; explicit `"agent"` preserved.
8. `conversation-store::ConversationHeader` round-trip: same.
9. `session-store::SessionHeader` round-trip: same.
10. `stamp_surface_mode` helper: `Some(Agent)` wins over profile;
    `None` falls back to profile membership; chat profile falls
    through to `Chat`; `Agent` only when profile is in the set.
11. `to_conversation_header` upgrades legacy `Chat`-defaulted rows
    whose `grant.profile` is in `agent_profiles` to `Agent` on the
    projected header (on-disk stays `Chat`).

### Operational

12. No `SessionGrant` field added. No `SessionKind` variant added. No
    on-disk migration: legacy rows have no `surface_mode` field; the
    field defaults to `Chat` on read.
13. No new dependency in `Cargo.toml`.
14. `cargo fmt --check`, `cargo clippy --workspace --exclude
    liberado-webui --all-targets -- -D warnings`, and
    `cargo test --workspace --no-fail-fast` all pass for the touched
    crates (S1 is narrow enough that the gate is local).
15. CRAP ratchet: any new helper lands below 30. Existing functions
    may not increase above baseline.

### Documentation

16. This plan is the canonical doc for the chat/agent split. The
    roadmap and backlog point at it.
17. `docs/future-work/tui-maturity-roadmap.md` carries one new
    sentence: shelf split deferred, TUI parity (kind filter) is not
    in the active slice order.


## 6a. Slice 2 + CAS1-followups acceptance criteria

**Status**: Slice 2 met in [#276](https://github.com/ForrestThump/liberado/pull/276); CAS1 follow-ups met in [#277](https://github.com/ForrestThump/liberado/pull/277) (data layer + three cross-PR consistency fixes). All criteria below pinned by tests; no manual checklist.

### Slice 2 (WebUI shelves + create-path + privileged `create_agent`)

1. WebUI sidebar renders two tabs: **Chats** (default), **Agents**. Switching shelves refilters the list without dropping the active conversation, even if it lives on the other shelf. Search hits are scoped to the active shelf; unknown ids (race / stale) stay visible rather than silently dropping.
2. `POST /api/conversations` with `{"profile": "<name>", "title"?: "..."}` opens an Agent-shelf chat under the named profile. Fail-closed on an unknown / disabled profile (HTTP 400 with the profile name in the message). `{}` opens a default-grant Chat (unchanged from S1).
3. `GET /api/profiles` reports `agent_eligible: true` for every profile in the deployment's `[chat] agent_profiles` set and `false` otherwise. WebUI New Agent picker uses this list, filtering for `domain`-absent chat profiles.
4. Privileged face tool `create_agent` is registered only when the *current* session's profile is an agent-creator (`is_agent_creator_profile(None | Some("operator"))` → `true`; everyone else → `false`). Rejects with a clear error naming the knob when `profile` is not in the deployment's set.
5. `ChatSessions::create_with_grant` flow → `stamp_surface_mode(grant, None, &self.agent_profiles)` → stamps `Agent` iff the named profile is in the deployment's set. No second stamp rule to drift.

### CAS1 follow-ups (data layer + cross-PR consistency)

6. `AgentProfiles` is deployment-tunable via `[chat] agent_profiles` in `tuning.toml`. Default set is `coding | life | researcher | operator` (`AgentProfiles::DEFAULT_NAMES`).
7. Boot wires the same `AgentProfiles` into both `ChatSessions` (`with_agent_profiles`) and `SessionStore` (`with_agent_profiles`). The boot log emits one info line per side reporting count + whether the conservative default is in force.
8. Every read path — `ChatSessions::stamp_surface_mode`, `ChatSessions::create_agent_chat`, `face::create_agent::parse_create_agent_args`, `SessionStore::to_conversation_header_with`, `GET /api/profiles` — consults the deployment's set, never the free function `is_agent_profile`. The five sites are enumerated in §6c.
9. `cargo fmt --check`, `cargo clippy --workspace --exclude liberado-webui --all-targets -- -D warnings`, full `cargo test --workspace --no-fail-fast`, the function-complexity ratchet, and the module-health ratchet all pass; the WebUI WASM release build passes.
10. No `SessionGrant` field added; no `SessionKind` variant added; no on-disk migration (legacy rows default to `Chat` and the chat-lens projection upgrades them per the deployment's set).


## 6b. Create-path + privileged agent spawn (stacked on CAS2)

Human and privileged-face paths that **create** Agent-shelf chats (Reading B stamp via
`create_with_grant`). Distinct from `delegate` / GoalSessionHub (dispatch-domain goal
sessions, often Chat on the shelf).

### Human — WebUI **New Agent**

1. Sidebar **New Agent** opens a picker of profiles where `GET /api/profiles` reports
   `agent_eligible: true` **and** `domain` is absent (chat hats only).
2. Create is `POST /api/conversations` with body `{ "profile": "<name>", "title"?: "..." }`.
   The daemon resolves the profile (`Config::resolve_session_profile`, fail closed) and
   calls `ChatSessions::create_with_grant`, so `stamp_surface_mode` returns `Agent` when
   the name is in `agent_profiles`.
3. After create: select the new conversation, switch shelf to Agents, refresh the list.
4. **New Chat** is unchanged — default-grant Chat (nonce / first message).

### Privileged face — `create_agent` tool

1. Built-in face tool alongside `delegate` (`CREATE_AGENT_TOOL_NAME`).
2. Args: `profile` (required, must be in the deployment's `[chat] agent_profiles` set),
   optional `title`. Opening-message enqueue is skipped (not cheap inside an in-flight
   turn); return the id.
3. Behavior: resolve profile → reject if **not in the deployment's `agent_profiles` set**
   (this is the same set the HTTP New Agent path and `stamp_surface_mode` consult) →
   `create_with_grant` → JSON `{ "conversation_id", "surface_mode": "agent", "profile", "title"? }`.
4. **Privilege gate A:** tool is only registered when the *current* session's profile is
   an agent-creator (`is_agent_creator_profile`): default face (`None`) or `operator`.
   Non-creators (`coding`, `life`, `researcher`, …) do not see the tool.
5. Creators cannot pass custom capability lists. No peer mesh. Child grant = only what the
   named profile resolves to in policy. `set_profile` / `POST .../profile` remains
   human-only HTTP — never a model tool.

### Not this path

- `delegate` — GoalSessionHub dispatch jobs; strips AskHuman; awaits terminal result.
- Mid-conversation `set_profile` — human authority switch, not create-time stamp.

## 6c. Every site reads the deployment's tuned set

`agent_profiles` is the deployment's single source of truth. **Every** create / read / pick path consults the same `AgentProfiles` instance threaded through from `config.tuning.chat.agent_profiles` at boot:

| Site | File | How it consults the set |
|---|---|---|
| `ChatSessions::stamp_surface_mode` (create-time) | `crates/main-agent/src/sessions/surface_mode.rs` | `self.agent_profiles.is_agent(profile)` — stamps `Agent` iff the named profile is in the deployment's set. |
| `ChatSessions::create_agent_chat` (face `create_agent` tool) | `crates/main-agent/src/sessions/agent_spawn.rs` | Same; rejects with `"profile X is not in the deployment's [chat] agent_profiles set"` (clear error, names the configuration knob). |
| `face::create_agent::parse_create_agent_args` | `crates/main-agent/src/face/create_agent.rs` | Validates **shape only** (non-empty after trim). The policy check lives in `create_agent_chat` next to the data it consults. |
| `SessionStore::to_conversation_header_with` (chat-lens projection) | `crates/session-store/src/types.rs` | Passed `&self.agent_profiles` on every `create`/`list`/`header` call. Legacy-row upgrade: a row whose on-disk `surface_mode` is `Chat` is projected as `Agent` iff `grant.profile ∈ agent_profiles`. |
| `GET /api/profiles` (`agent_eligible` field) | `crates/server/src/api/chat.rs` | Reports `agent_eligible: true` iff `name ∈ config.tuning.chat.agent_profiles`. The WebUI New Agent picker uses this to filter. |

The free function `is_agent_profile(name)` is kept as a backward-compat shortcut for `AgentProfiles::default().is_agent(name)`; it is **not** the source of truth for any of the five sites above. Adding a deployment hat means adding it to `[chat] agent_profiles` — no code change.

The cross-PR consistency was established by three follow-up commits on top of PR #277:
1. `fix(face): honor deployment's [chat] agent_profiles in create_agent tool` — the parser drops the policy check; `create_agent_chat` reads `self.agent_profiles`.
2. `fix(api): /api/profiles reports agent_eligible from the deployment's set` — the picker now offers what the deployment actually accepts.
3. `fix(session-store): wire deployment's [chat] agent_profiles at boot` — the `SessionStore` builder chain was unused in production; the boot now calls `with_agent_profiles` so the chat-lens projection reflects the deployment's set.

Conformance tests pin all three: `create_agent_chat_honours_deployment_agent_profiles` (face), `projection_uses_with_agent_profiles_when_wired` (storage). The end-to-end story is: a deployment that lists `{coding, designer}` in `[chat]` gets the same `Agent`/`Chat` answer from all five sites, with the same error message when a name is missing.

## 7. Follow-ups

- **S3 (chat-default tools, Slice 3)** — **NEXT.** Name a `chat-default` profile in
  `config.example/topology.toml` and add `chat-search` (read-only MCP) to the
  `main-agent` grant as a commented block in `config.example/policy.toml`.
  One PR; the design is settled in §5.
- **S4 (dogfood, Slice 4)** — Blocked on S3. One week of measured shelves +
  chat-default, then tune `agent_profiles` from observed usage. The
  follow-up is the gate to JEV1 / CAS5 — see [`jev-integration.md`](jev-integration.md) §10.
- **Jev (later)** — dispatcher-side "which agent does this belong to"
  wedge. Full phase plan: [`jev-integration.md`](jev-integration.md).
  Lands after S4 dogfood.
- **TUI parity (later)** — `K` key filter on `SessionKind`. Small, but
  explicitly deferred until shelves are stable.

**Already done (in the slice chain this plan locks):**
- **S1 (wire + stamp)** — landed in [#274](https://github.com/ForrestThump/liberado/pull/274).
- **S2 (WebUI shelves + create-path + privileged `create_agent`)** — landed in [#276](https://github.com/ForrestThump/liberado/pull/276). WebUI Chats | Agents shelves, **New Agent** (`POST /api/conversations` with profile), privileged face `create_agent` (gate A: default face / `operator`).
- **CAS1 follow-ups (data layer + cross-PR consistency)** — landed in [#277](https://github.com/ForrestThump/liberado/pull/277). `AgentProfiles` is now a deployment-tunable set wired through `ChatSessions` and `SessionStore`; the five sites in §6c all read from it.
