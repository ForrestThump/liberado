---
kind: validation
status: active
authority: evidence
domain: correctness
open_items: false
---

# Validation

Correctness artifacts for the Liberado workspace: mutation-testing reports, coverage analysis, and test-infrastructure design.

**Summary document — start here:** [`mutation-testing-plan.md`](mutation-testing-plan.md). It holds the master plan and Phase 1–5 results across all 13 hardened crates (run order, methodology, catch rates, survivor triage, Phase 5 roadmap). The per-crate reports below are supporting detail.

## Evidence provenance (required fields)

Each result record should carry:

| Field | Meaning |
|-------|---------|
| `commit` | Exact git commit of the tree under test |
| `date` | When the run finished (ISO date) |
| `command` | Exact command line |
| `os_env` | OS and important environment facts |
| `tool_version` | Tool and model version (e.g. cargo-mutants, rustc) |
| `mutation` | Mutation applied, if any |
| `artifact` | Path or digest of raw output |
| `conclusion` | What was proved |
| `currency` | `current` (still a guarantee) or `historical` (true at that revision) |

Tests are **current** executable evidence. A mutation report is **historical** evidence that a test caught a defect at a particular revision.

## Layout

- **Per-crate reports:** [`mutation-testing/`](mutation-testing/) — one report per hardened crate (Phase 1 + Phase 4) plus the original 2026-07-23 first-pass report.
- **Summary:** [`mutation-testing-plan.md`](mutation-testing-plan.md) — the aggregate plan + results that ties the per-crate reports together.
- **Correctness artifacts:** [`coverage-gaps.md`](coverage-gaps.md) — known uncovered code paths (IO/network/clock-gated, tracing-only, defaults).
- The mock-harness design ([`impl/mock-harness-scope.md`](../impl/mock-harness-scope.md)) is a forward design plan ("what to build"), not a correctness artifact, so it lives in `docs/impl/`.

| Doc | Role |
|-----|------|
| [mutation-testing-plan.md](mutation-testing-plan.md) | **Summary** — master plan + Phase 1–5 results across 13 crates |
| [mutation-testing/](mutation-testing/) | Per-crate mutation-testing reports (Phase 1 + Phase 4) |
| [coverage-gaps.md](coverage-gaps.md) | Known uncovered code paths — IO/network/clock-gated, tracing-only, defaults |
| [impl/mock-harness-scope.md](../impl/mock-harness-scope.md) | Test-infrastructure design — scriptable error mocks, FrozenClock, filesystem stubs |

## Per-crate reports ([`mutation-testing/`](mutation-testing/))

| Report | Role |
|--------|------|
| [mutation-testing-report-phase0-2026-07-23.md](mutation-testing/mutation-testing-report-phase0-2026-07-23.md) | Original first-pass run across 10 crates (2026-07-23) |
| [mutation-testing-report-dispatcher.md](mutation-testing/mutation-testing-report-dispatcher.md) | Dispatcher crate detail — 96.4% catch rate; dispatcher-only, not a true "v2" of the program |
| [mutation-testing-report-provider.md](mutation-testing/mutation-testing-report-provider.md) | Provider crate detail (Phase 1) |
| [mutation-testing-report-common.md](mutation-testing/mutation-testing-report-common.md) | Common crate detail (Phase 1) |
| [mutation-testing-report-config-loader.md](mutation-testing/mutation-testing-report-config-loader.md) | Config-loader crate detail (Phase 1) |
| [mutation-testing-report-session.md](mutation-testing/mutation-testing-report-session.md) | Session crate detail (Phase 1) |
| [mutation-testing-report-config.md](mutation-testing/mutation-testing-report-config.md) | Config crate detail (Phase 1) |
| [mutation-testing-report-executor.md](mutation-testing/mutation-testing-report-executor.md) | Executor crate detail (Phase 1) |
| [mutation-testing-report-orchestrator.md](mutation-testing/mutation-testing-report-orchestrator.md) | Orchestrator crate detail (Phase 1) |
| [mutation-testing-report-coder-sandbox.md](mutation-testing/mutation-testing-report-coder-sandbox.md) | Coder-sandbox crate detail (Phase 4) |
| [mutation-testing-report-coder-tools.md](mutation-testing/mutation-testing-report-coder-tools.md) | Coder-tools crate detail (Phase 4) |
| [mutation-testing-report-daemon.md](mutation-testing/mutation-testing-report-daemon.md) | Daemon crate detail (Phase 4) |
| [mutation-testing-report-server.md](mutation-testing/mutation-testing-report-server.md) | Server crate detail (Phase 4) |
| [mutation-testing-report-coder-agent.md](mutation-testing/mutation-testing-report-coder-agent.md) | Coder-agent crate detail (Phase 4) |

**Naming notes:** `mutation-testing-report-v2.md` was renamed to `mutation-testing-report-dispatcher.md`. It covers only the dispatcher crate, not a second version of the overall mutation-testing program. `mutation-testing-report.md` — the original first-pass report — was renamed `mutation-testing-report-phase0-2026-07-23.md` to distinguish it from the aggregate plan.

**Related:** [coverage-gaps.md](coverage-gaps.md) for known uncovered code paths; [impl/mock-harness-scope.md](../impl/mock-harness-scope.md) for test infrastructure design; [docs/README.md](../README.md) for the full docs map.
