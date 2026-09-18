---
kind: plan
status: active
authority: implementation
domain: product
canonical_for: jev-integration
open_items: true
---

# Jev integration (post chat|agent shelves)

**Status**: blocked on CAS4 dogfood (≥1 week). This is the canonical phase plan for
TypeSafe Jev inside Liberado. It does **not** change CAS1–CAS4; those stay in
[`chat-agent-surface-mode.md`](chat-agent-surface-mode.md).

**Related**: [`chat-agent-surface-mode.md`](chat-agent-surface-mode.md) (Reading B +
CAS1–CAS4) · [`../../future-work/backlog.md`](../../future-work/backlog.md) (JEV1 / CAS5) ·
[`../../roadmap.md`](../../roadmap.md) (near-term callout).

## 1. Product north star

Liberado should feel like two **soft** surfaces, not one undifferentiated inbox:

1. **Chat** — LibreChat / Grok-*app* feel. Branchable threads, history, look-back,
   light tools (for example web search). Not zero tools; not a hard lockout.
2. **Agent** — Grok-*Bot* feel. Long-lived specialist contexts the operator keeps
   open and talks to over days. Existing agent tool curation and narrow grants stay
   as they are.

You can still chat with agents. Chat can still use some tools. The split is what
you **select when you start**, plus how the UI **shelves** those sessions — not a
capability firewall.

Steal Grok Bot paradigms that help: specialized long-lived contexts, clear shelves,
confidence-gated routing hints. Do **not** steal peer-to-peer agent mesh / A2A
authority. Kernel rules stay: capability `∩`, **dispatcher sole grantor**,
`SessionGrant` resolved once and never widened by peers.

Prefer a gated **slim** Liberado build for day-to-day work. Do not introduce
alternate product branding in this plan.

## 2. Why structure before Jev

There are many ways to wire TypeSafe Jev (Choice / Noul / Score). Highest leverage
is to polish **one** wedge, learn, then expand. Structure must exist first so Jev
has **addressable shelves and named specialist profiles** to route against — not a
free-floating "maybe spawn a goal."

Jev is **not** a second grantor. It advises; the dispatcher still grants.

## 3. Order of work (do not reorder)

Already reflected in backlog CAS1–CAS4 + TUI deferral:

1. **Tagging / wire stamp** (`surface_mode` on headers) — CAS1 / Slice 1 — in flight
2. **WebUI shelf split** (Chats / Agents, client-side partition) — CAS2
3. **Chat-default tools** (named profile + light tools; agent curation unchanged) — CAS3
4. **Dogfood + measure** shelves for ≥1 week; tune `agent_profiles` — CAS4
5. **TUI shelf/filter parity** — deferred until shelves are stable (Forrest does not
   use the TUI day-to-day). See
   [`../../future-work/tui-maturity-roadmap.md`](../../future-work/tui-maturity-roadmap.md).
6. **Jev** — only after structure is solid and CAS4 dogfood has run — **JEV1 / CAS5**

One PR under active review at a time. No CRAP/MH baseline raises — split functions
or extract tests instead.

## 4. Reading B reminder

**Reading B** (locked 2026-09-18): agent ≡ long-lived specialist **chat**. Stamp
`surface_mode` from create-time / profile class (`agent_profiles`), **not** from
`goal.is_some()`.

Reading A (agent ≡ `goal.is_some()`) is rejected: it shelves every `/spawn` and
misses the goal-less chat-under-`coding`-profile case the product names.

Full reasoning and Slice 1 acceptance criteria live in
[`chat-agent-surface-mode.md`](chat-agent-surface-mode.md). This document assumes
that stamp and those shelves already exist.

## 5. Kernel constraints (non-negotiable)

| Constraint | Rule |
|---|---|
| Dispatcher | **Sole grantor.** Jev never issues, widens, or rewrites a `SessionGrant`. |
| Jev role | Confidence-gated **advisor** only. Soft hints the UI / dispatcher may act on. |
| Peer mesh | **No** peer-to-peer agent messaging / A2A authority. Already rejected elsewhere. |
| `SessionGrant` | **No** `kind` / `surface_mode` field. The shelf stamp is not authority. |
| Soft mode | Chat stays able to answer locally; hints are not a hard lockout. |

## 6. TypeSafe Jev — first wedge

**What Jev is here:** TypeSafe AI's System One decision API (Choice / Noul / Score)
used as a confidence-gated advisor inside Liberado — not a peer agent, not a mesh
member, not a grant authority.

**First wedge (highest leverage; do this first):**

From **ambient Chat**, given a user prompt:

1. **Belong?** Does this look like it belongs with an existing Agent-shelf session
   or named specialist profile? (Choice / Noul)
2. **Which?** If yes, which agent / profile? (Choice over `agent_profiles` + live
   Agent-shelf sessions)
3. **Tool hints?** Optional soft hint of tools the agent would likely need if they
   are not already in its grant/context. The agent may still list or search tools —
   hints are not a hard tool lock.
4. **Output validation (stretch of the same wedge):** Score / Noul on whether a
   proposed reply or tool plan looks on-distribution for that surface.

## 7. Explicit non-goals for the first Jev PR

- Peer agent messaging / A2A mesh
- Jev as grantor or grant widener
- Replacing the dispatcher or executor
- Full model-router rewrite (model selection is a **later** Jev wedge)
- Hard routing that removes Chat's ability to answer locally
- Growing `SessionKind` or putting `surface_mode` on `SessionGrant`
- Pulling TUI shelf/filter work into the first Jev PR

## 8. Later wedges (after the first is polished)

- Model selection via Choice / Score
- Broader tool selection (still soft hints under the dispatcher grant)
- Deeper output validation across Agent sessions
- Ambient → spawn suggestions when no matching Agent exists

## 9. Soft mode rules (remember)

- Soft split, not lockout.
- Chat defaults light; Agent keeps curated grants.
- UI shelves by `ConvHeader.surface_mode`.
- Stamp at create; legacy rows default `chat`; projection may upgrade when
  `grant.profile ∈ agent_profiles`.
- Never put `surface_mode` on `SessionGrant`. Never grow `SessionKind` for this.

## 10. Gate to start JEV1 / CAS5

Blocked until:

1. CAS1–CAS3 have landed.
2. CAS4 dogfood has run for **at least one week** with measured shelf usage.
3. `agent_profiles` has been tuned from that dogfood (or explicitly left as-is with
   a short note in the CAS4 write-up).

Until then, keep this document as the north-star phase plan and keep implementing
CAS slices one PR at a time.
