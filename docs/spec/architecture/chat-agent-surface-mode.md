---
kind: plan
status: active
authority: implementation
domain: product
canonical_for: chat-agent-surface-mode
open_items: true
---

# Chat vs agent surface mode

**Status**: Slice 1 (wire + stamp) landed in [#274](https://github.com/ForrestThump/liberado/pull/274). Slice 2 (WebUI shelves) is in flight. Slices 3–4 follow as separate
PRs. Jev is out of scope for **this** document (CAS1–CAS4 only); the full phase
plan lives in [`jev-integration.md`](jev-integration.md) and starts only after
the shelves are dogfooded.

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

`agent_profiles` is a small const set documented in the plan and pinned
in code as a single `fn is_agent_profile(name: &str) -> bool` so adding
a name is one edit:

```rust
// crates/main-agent/src/sessions/surface_mode.rs
fn is_agent_profile(name: &str) -> bool {
    matches!(name, "coding" | "life" | "researcher" | "operator")
}
```

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

| Slice | Scope | PR size |
|---|---|---|
| **S1 — wire stamp** | Add `SurfaceMode`, stamp at create paths, persist on headers, expose on `ConvHeader`. Tests for serde roundtrip and stamp logic. **This PR.** | one |
| **S2 — WebUI shelves** | Two top-level shelves inside the sidebar (Chats / Agents), partition client-side by `ConvHeader.surface_mode`. Default to Chats. | one |
| **S3 — chat-default tools** | Named `chat-default` `[[session_profiles]]` entry; `chat-search` (read-only MCP) granted to `main-agent`. Operator opt-in via `policy.toml` comment block. | one |
| **S4 — dogfood + measure** | One-week dogfood of shelves + chat-default. Tune `agent_profiles` set from observed usage. | one |
| **later — Jev** | Out of scope for this plan. See [`jev-integration.md`](jev-integration.md) for the phase plan (first wedge, non-goals, kernel constraints). Lands after S4 has measured the shelves for a week. |

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

## 7. Follow-ups (NOT in this PR)

- **S2 (WebUI shelves)**: split `crates/webui/src/components/sidebar.rs`
  into Chats / Agents tabs, default to Chats, partition client-side by
  `ConvHeader.surface_mode`.
- **S3 (chat-default tools)**: name `chat-default` profile in
  `config.example/topology.toml`, add `chat-search` to the `main-agent`
  grant as a commented block in `config.example/policy.toml`.
- **S4 (dogfood)**: one week of measured shelves + chat-default, then
  tune `agent_profiles` from observed usage.
- **Jev (later)**: dispatcher-side "which agent does this belong to"
  wedge; lands after S4 has been dogfooded. Full phase plan:
  [`jev-integration.md`](jev-integration.md).
- **TUI parity (later)**: `K` key filter on `SessionKind` — small, but
  explicitly deferred until shelves are stable.
