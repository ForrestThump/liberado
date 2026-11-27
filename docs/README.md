---
kind: index
status: active
authority: advisory
domain: docs
---

# Liberado documentation

**Single source of truth** for humans and agents. Liberado is a Rust-native **agentic orchestration** system: one daemon, capability-bounded tools (MCP), domain packs (life-ops, coding), and thin surfaces (TUI, WebUI, CLI, Telegram).

If you are an agent: start at the [roadmap](roadmap.md) → [architecture overview](spec/architecture/overview.md) → [failure modes](spec/architecture/failure-modes.md). Read the failure modes document before changing safety or tests.

**Authority model:** [doc-authority.md](spec/reference/doc-authority.md). **Catalog:** [CATALOG.md](CATALOG.md). **ADRs:** [decisions/](decisions/README.md).

---

## Authority table

| Question | Authority |
|---|---|
| What does the program do now? | Code, tests, and crate Rustdoc |
| What must remain true across crates? | `docs/spec/reference` and `docs/spec/architecture/contracts.md` |
| Why was this design selected? | Accepted ADR under [decisions/](decisions/README.md) |
| What should an agent implement next? | [roadmap.md](roadmap.md) only |
| What is the product mission and strategic direction? | [roadmap.md](roadmap.md) |
| What is the broader direction and status snapshot? | [roadmap.md](roadmap.md). It holds the direction and the work queue. |
| What did an experiment prove? | Dated record under [validation/](validation/README.md) |
| What might we do later? | Proposed plan under [future-work/](future-work/README.md) |
| What happened before? | [future-work/archive/](future-work/archive/README.md) and git history |

---

## Folder structure

| Folder | Contents |
|--------|----------|
| **`spec/`** | Design specs, living architecture narrative, reference docs — the truth about how the system works |
| **`decisions/`** | Architecture Decision Records (mostly immutable; supersede rather than edit) |
| **`impl/`** | Developer guides, setup, contribution workflow — how to build and work here |
| **`future-work/`** | Forward-looking: plans, ideas, research, historical archives — design material, not the queue |
| **`validation/`** | Correctness: mutation testing reports, coverage analysis |
| **`CATALOG.md`** | Generated repository-wide document catalog |

---

## Cold-start (humans & agents)

1. [Roadmap](roadmap.md) — product direction and the work queue
2. [Architecture overview](spec/architecture/overview.md) — pillars, daemon-first loop, safety
3. [Sessions](spec/architecture/sessions.md) — everything is a `Session` (D7)
4. [Contracts](spec/architecture/contracts.md) — narrow waists / frozen seams
5. [Failure modes](spec/architecture/failure-modes.md) — seven recurring bug classes
6. [Contributor orientation](../AGENTS.md) — build, test, architecture, and repository rules
7. [Developer workflow](impl/development-workflow.md) — how work gets done here

Per-crate detail: the generated [crate map](spec/reference/crate-map.md) lists every crate, its layer role, and its internal dependencies. Some crates also carry a hand-written `crates/*/ARCHITECTURE.md`.

---

## Key documents

| Document | Path | Purpose |
|----------|------|---------|
| **Roadmap** | [roadmap.md](roadmap.md) | Product direction, and the only work queue |
| **Doc authority** | [spec/reference/doc-authority.md](spec/reference/doc-authority.md) | What each document may claim |
| **Failure modes** | [spec/architecture/failure-modes.md](spec/architecture/failure-modes.md) | Seven recurring bug classes |
| **Architecture decisions** | [decisions/README.md](decisions/README.md) | ADRs (why this design) |
| **Config spec** | [spec/config-spec.md](spec/config-spec.md) | TOML config file contract |
| **Dispatch logic** | [spec/dispatch-logic-spec.md](spec/dispatch-logic-spec.md) | Dispatcher design |
| **API reference** | [spec/reference/api.md](spec/reference/api.md) | HTTP/SSE surface |
| **Tuning** | [spec/reference/tuning.md](spec/reference/tuning.md) | All behavioral knobs |
| **Mutation testing** | [validation/mutation-testing-plan.md](validation/mutation-testing-plan.md) | Plan, plus per-crate reports under [validation/mutation-testing/](validation/mutation-testing/) |
| **Coverage gaps** | [validation/coverage-gaps.md](validation/coverage-gaps.md) | Historical llvm-cov snapshot (2026-07-29); see `mutation-testing-plan.md` for the executable guarantee |
| **Invariants** | [spec/architecture/failure-modes.md](spec/architecture/failure-modes.md) §6 | "Two things that should agree" |

---

## Product frame

Personal assistant first: the data, the rules, and reliable delivery. The coding pack is frozen. For what to build next, and for the product direction, read the [roadmap](roadmap.md).

**Recently hardened (2026-07-30):** a mutation testing pass over the core crates (per-crate reports under [validation/mutation-testing/](validation/mutation-testing/)), Tier-1 conformance suite (L1–L11), dual-guard conformance tests, negative-case API tests, cargo-deny CI gate, session state-machine invariants, JSONL rehydration fuzzing, provider wire-body seam tests. See [validation/](validation/).

---

## Link conventions (GitHub)

- Prefer **relative** links from the linking file.
- Do not use site-root paths like `/docs/...` (they break on GitHub blob views).
- Archive pages must not be linked as "current" architecture without a status banner.

**Checking links:** run `just check-links` (or `cargo run --locked -p liberado-cli -- docs check-links`) to verify every relative markdown link in `docs/`, the repo-root `README.md`, and every `crates/*/ARCHITECTURE.md` resolves to a real file, resolved from each linking file's directory. External `http(s)`/protocol URLs and `.secret` files are skipped, so the check never needs network access. CI enforces the same check in the `doc-links` job.

**Last updated:** 2026-11-27 — the roadmap is now the single source of truth for direction and the work queue. The 2026-09-18 archive of `docs/project/handoff.md` and `docs/project/design_questions.md` into `docs/future-work/archive/` still stands.
