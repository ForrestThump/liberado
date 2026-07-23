# Design questions for the user

Open product/docs conflicts or ambiguities found during the 2026-07-23 docs reorg and architecture-hardening pass. **Agents must not invent answers** — park decisions here until you resolve them.

Each item points at the docs (or code areas) that disagree or leave a gap.

---

## 1. Homelab deploy of architecture-hardening branch

**Question:** When should `architecture-hardening` (module splits, T1 suite, MCP pooling) be built and redeployed to the homelab as `liberado:dev`?

**Why it matters:** [handoff.md](handoff.md) still describes the 2026-07-19 image as the ops truth; pooling and T1 only help production after deploy.

**Related:** [roadmap/current.md](roadmap/current.md), [handoff.md](handoff.md).

---

## 2. M1 remaining scope: registry UX vs degraded catalog

**Question:** After pooling landed, is the next MCP ops slice **registry UX** (beyond hand-edited TOML), **degraded-peer catalog + dispatcher avoidance**, or both in a fixed order?

**Related:** [roadmap/current.md](roadmap/current.md) (M1b), [crates/mcp/ARCHITECTURE.md](../crates/mcp/ARCHITECTURE.md) (via crate), research baseline [research/archive/grok-architecture-analysis-2026-07-22.md](research/archive/grok-architecture-analysis-2026-07-22.md).

---

## 3. Spec vs living architecture as source of truth

**Question:** When a file under [specs/](specs/README.md) disagrees with [architecture/](architecture/README.md) or with code (e.g. older conversation-store wording vs D7 `SessionStore`), should the default rule be **code + architecture README wins**, with specs marked “historical depth only”?

**Related:** [specs/README.md](specs/README.md), [specs/liberado-conversation-store-spec.md](specs/liberado-conversation-store-spec.md), [architecture/sessions.md](architecture/sessions.md), [architecture/contracts.md](architecture/contracts.md).

---

## 4. Messaging extraction timing

**Question:** Is finishing Telegram extraction behind `liberado-messaging` (sticky + free-form out of server composition root) still deferred until after C1/T1 remainder, or should it block multi-channel work?

**Related:** [architecture/channels-and-interactivity.md](architecture/channels-and-interactivity.md), [ideas/matrix-chat-surface.md](ideas/matrix-chat-surface.md), server modules `sticky` / `telegram`.

---

## 5. Coding pack priority vs “not replacing Claude Code”

**Question:** [positioning.md](architecture/positioning.md) and [roadmap/current.md](roadmap/current.md) deprioritize coding polish, but [rust-native-agentic-coder-plan.md](roadmap/rust-native-agentic-coder-plan.md) and [pr-dispatch-vtcode-no-write-finding.md](roadmap/pr-dispatch-vtcode-no-write-finding.md) still look like large active programs. Confirm coding remains **integration-only** until P1 daily-drive bar is met.

**Related:** those files + [architecture/agentic-loops.md](architecture/agentic-loops.md).

---

## 6. Ideas vs commitments

**Question:** Should anything still in [ideas/](ideas/README.md) (e.g. turn-budget battery, Matrix) be promoted to an active roadmap row in this quarter, or stay pure backlog?

**Related:** [ideas/turn-budget-battery-idea.md](ideas/turn-budget-battery-idea.md), [ideas/matrix-chat-surface.md](ideas/matrix-chat-surface.md), [roadmap/latency-and-routing-observability-plan.md](roadmap/latency-and-routing-observability-plan.md).

---

*Created 2026-07-23 during enterprise docs reorg.*
