> ⚠️ Archive (moved 2026-09-18 from `docs/project/design_questions.md`). Last updated 2026-07-23.
> Item #2 is **closed** in the original. Items #3 and #6 are now answered by
> [`../../spec/reference/doc-authority.md`](../../spec/reference/doc-authority.md) and
> [`../../roadmap.md`](../../roadmap.md) ("Deliberately not scheduled" section). Remaining
> items #4 and #5 are stale operational diary and are kept for historical reference only.

# Design questions for the user

Open product/docs conflicts or ambiguities found during the 2026-07-23 docs reorg and architecture-hardening pass. **Agents must not invent answers** — park decisions here until you resolve them.

Each item points at the docs (or code areas) that disagree or leave a gap.

---

## 1. Homelab deploy of architecture-hardening branch

**Question:** When should `architecture-hardening` (module splits, T1 suite, MCP pooling) be built and redeployed to the homelab as `liberado:dev`?

**Why it matters:** [project-handoff-2026-07.md](project-handoff-2026-07.md) still described the 2026-07-19 image as the ops truth; pooling and T1 only help production after deploy. (Both files are now in the archive.)

**Related:** [../roadmap.md](../../roadmap.md), [project-handoff-2026-07.md](project-handoff-2026-07.md) (sibling archive).

---

## 2. M1 remaining scope: registry UX vs degraded catalog

**Status (2026-07-23):** **Closed.** Degraded-catalog routing landed; topology MCP **hot-reload**
landed (`apply_mcp_peer_set` / `POST /api/mcp/reload`). No product registry UI; no agent-owned
MCPs; hand-edited `topology.toml` remains the operator surface.

---

## 3. Spec vs living architecture as source of truth

**Question:** When a file under [../spec/README.md](../../spec/README.md) disagrees with [architecture/](../../spec/architecture/README.md) or with code (e.g. older conversation-store wording vs D7 `SessionStore`), should the default rule be **code + architecture README wins**, with specs marked “historical depth only”?

**Related:** [../spec/README.md](../../spec/README.md), [../spec/conversation-store-spec.md](../../spec/conversation-store-spec.md), [architecture/sessions.md](../../spec/architecture/sessions.md), [architecture/contracts.md](../../spec/architecture/contracts.md).

---

## 4. Messaging extraction timing

**Question:** Is finishing Telegram extraction behind `liberado-messaging` (sticky + free-form out of server composition root) still deferred until after C1/T1 remainder, or should it block multi-channel work?

**Related:** [architecture/channels-and-interactivity.md](../../spec/architecture/channels-and-interactivity.md), [ideas/matrix-chat-surface.md](../../future-work/ideas/matrix-chat-surface.md), server modules `sticky` / `telegram`.

---

## 5. Coding pack priority vs “not replacing Claude Code”

**Question:** [positioning.md](../../spec/architecture/positioning.md) and [../roadmap.md](../../roadmap.md) deprioritize coding polish, but [rust-native-agentic-coder-plan.md](../../future-work/rust-native-agentic-coder-plan.md) and [pr-dispatch-vtcode-no-write-finding.md](../../future-work/archive/pr-dispatch-vtcode-no-write-finding.md) still look like large active programs. Confirm coding remains **integration-only** until P1 daily-drive bar is met.

**Related:** those files + [architecture/agentic-loops.md](../../spec/architecture/agentic-loops.md).

---

## 6. Ideas vs commitments

**Question:** Should anything still in [ideas/](../../future-work/ideas/README.md) (e.g. turn-budget battery, Matrix) be promoted to an active roadmap row in this quarter, or stay pure backlog?

**Related:** [ideas/turn-budget-battery-idea.md](../../future-work/ideas/turn-budget-battery-idea.md), [ideas/matrix-chat-surface.md](../../future-work/ideas/matrix-chat-surface.md), [../future-work/latency-and-routing-observability-plan.md](../../future-work/latency-and-routing-observability-plan.md).

---

*Created 2026-07-23 during enterprise docs reorg.*
