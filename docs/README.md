# Liberado documentation

**Single source of truth** for humans and agents. Liberado is a Rust-native **agentic orchestration** system: one daemon, capability-bounded tools (MCP), domain packs (life-ops, coding), and thin surfaces (TUI, WebUI, CLI, Telegram).

If you are an agent: start at the [roadmap](roadmap.md) → [architecture overview](spec/architecture/overview.md) → [failure modes](spec/architecture/failure-modes.md). Read the failure modes document before changing safety or tests.

---

## Folder structure

| Folder | Contents |
|--------|----------|
| **`spec/`** | Design specs, living architecture narrative, reference docs — the truth about how the system works |
| **`impl/`** | Developer guides, setup, contribution workflow — how to build and work here |
| **`future-work/`** | Forward-looking: plans, ideas, research, historical archives — what might happen next |
| **`validation/`** | Correctness: mutation testing reports, coverage analysis |
| **`project/`** | Meta: handoffs, design questions |

---

## Cold-start (humans & agents)

1. [Roadmap](roadmap.md) — what is open next
2. [Architecture overview](spec/architecture/overview.md) — pillars, daemon-first loop, safety
3. [Sessions](spec/architecture/sessions.md) — everything is a `Session` (D7)
4. [Contracts](spec/architecture/contracts.md) — narrow waists / frozen seams
5. [Failure modes](spec/architecture/failure-modes.md) — six recurring bug classes
6. [Build & run](impl/AGENTS.md) — workspace layout, commands, configuration
7. [Developer workflow](impl/development-workflow.md) — how work gets done here
8. [Handoff](project/handoff.md) — what is live on the homelab today

Per-crate detail: generated [crate map](spec/reference/crate-map.md) + each crate's `crates/*/ARCHITECTURE.md`.

---

## Key documents

| Document | Path | Purpose |
|----------|------|---------|
| **Roadmap** | [roadmap.md](roadmap.md) | Living single-page roadmap |
| **Failure modes** | [spec/architecture/failure-modes.md](spec/architecture/failure-modes.md) | Six recurring bug classes |
| **Architecture decisions** | [spec/architecture-decisions.md](spec/architecture-decisions.md) | Frozen design decisions (ADRs) |
| **Config spec** | [spec/config-spec.md](spec/config-spec.md) | TOML config file contract |
| **Dispatch logic** | [spec/dispatch-logic-spec.md](spec/dispatch-logic-spec.md) | Dispatcher design |
| **API reference** | [spec/reference/api.md](spec/reference/api.md) | HTTP/SSE surface |
| **Tuning** | [spec/reference/tuning.md](spec/reference/tuning.md) | All behavioral knobs |
| **Mutation testing** | [validation/mutation-testing-plan.md](validation/mutation-testing-plan.md) | Plan + results across 13 crates |
| **Coverage gaps** | [validation/coverage-gaps.md](validation/coverage-gaps.md) | Known uncovered paths |
| **Invariants** | [spec/architecture/failure-modes.md](spec/architecture/failure-modes.md) §6 | "Two things that should agree" |

---

## Product frame

Liberado is sequenced **daemon (life-ops) → chat surface → coding pack**, not "three products at once."

**Recently hardened (2026-07-30):** complete mutation testing pass (13 crates), Tier-1 conformance suite (L1–L11), dual-guard conformance tests, negative-case API tests, cargo-deny CI gate, session state-machine invariants, JSONL rehydration fuzzing, provider wire-body seam tests. See [roadmap.md](roadmap.md) and [validation/](validation/).

---

## Link conventions (GitHub)

- Prefer **relative** links from the linking file.
- Do not use site-root paths like `/docs/...` (they break on GitHub blob views).
- Archive pages must not be linked as "current" architecture without a status banner.

**Checking links:** run `just check-links` (or `powershell -File scripts/check-doc-links.ps1` directly) to verify every relative markdown link in `docs/`, the repo-root `README.md`, and every `crates/*/ARCHITECTURE.md` resolves to a real file, resolved from each linking file's directory. External `http(s)`/protocol URLs and `.secret` files are skipped, so the check never needs network access. CI enforces the same check in the `doc-links` job.

**Last updated:** 2026-07-31 — docs reorganized per `project/` → `spec/impl/future-work/validation/project` schema; `just check-links` doc-link checker added.
