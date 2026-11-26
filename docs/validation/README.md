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
- **Correctness artifacts:** [`coverage-gaps.md`](coverage-gaps.md) — **historical snapshot** (2026-07-29, 8 hardened crates); the "Real Logic Gaps" section is closed, and remaining categories are infrastructure-bound. See `mutation-testing-plan.md` for the executable guarantee.
- [`coverage-gaps-coding-features-2026-08-06.md`](coverage-gaps-coding-features-2026-08-06.md) — **historical snapshot** (2026-08-06, develop vs main, ~6,470 new lines across 55 files); was never indexed under `future-work/`. High-priority gaps were tracked forward through the coding-harness reliability work.
- The completed mock-harness design is retained in the
  [future-work archive](../future-work/archive/mock-harness-scope.md). Current behavior lives in
  the test-helper code, Rustdoc, and tests.

| Doc | Role |
|-----|------|
| [mutation-testing-plan.md](mutation-testing-plan.md) | **Summary** — master plan + Phase 1–5 results; the per-crate reports cover 30 crates plus the Phase 0 first-pass report (31 files) |
| [mutation-testing/](mutation-testing/) | Per-crate mutation-testing reports (Phase 1 + Phase 4) |
| [coverage-gaps.md](coverage-gaps.md) | **Historical snapshot (2026-07-29)** — llvm-cov output for 8 hardened crates; "Real Logic Gaps" closed, remaining categories are infrastructure-bound |
| [coverage-gaps-coding-features-2026-08-06.md](coverage-gaps-coding-features-2026-08-06.md) | **Historical snapshot (2026-08-06)** — develop vs main, ~6,470 new lines; high-priority gaps tracked forward through coding-harness reliability work |
| [future-work/archive/mock-harness-scope.md](../future-work/archive/mock-harness-scope.md) | Historical test-infrastructure design — scriptable error mocks, FrozenClock, filesystem stubs |

## Per-crate reports ([`mutation-testing/`](mutation-testing/))

The directory holds 31 files: 30 per-crate reports plus the Phase 0 first-pass report. The first
table below lists the Phase 0, Phase 1, and Phase 4 reports; the later campaigns follow.

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

### Later campaigns (August–September 2026)

| Report | Role |
|--------|------|
| [mutation-testing-report-acp-bridge.md](mutation-testing/mutation-testing-report-acp-bridge.md) | Acp-bridge crate detail (recorded 2026-08-24) |
| [mutation-testing-report-bootstrap.md](mutation-testing/mutation-testing-report-bootstrap.md) | Bootstrap crate detail (recorded 2026-08-25) |
| [mutation-testing-report-chat-client-contract.md](mutation-testing/mutation-testing-report-chat-client-contract.md) | Chat-client-contract crate detail (recorded 2026-08-28) |
| [mutation-testing-report-chat-search.md](mutation-testing/mutation-testing-report-chat-search.md) | Chat-search crate detail (updated 2026-08-23) |
| [mutation-testing-report-coder-core.md](mutation-testing/mutation-testing-report-coder-core.md) | Coder-core crate detail (ledger campaign at `4857a35`) |
| [mutation-testing-report-coder-runner.md](mutation-testing/mutation-testing-report-coder-runner.md) | Coder-runner crate detail (campaign commit `ce1db7a5`) |
| [mutation-testing-report-cost.md](mutation-testing/mutation-testing-report-cost.md) | Cost crate detail (ledger rows at `425c58ad` and `b59f208d`) |
| [mutation-testing-report-eval.md](mutation-testing/mutation-testing-report-eval.md) | Eval crate detail (campaign commit `b4686b87`) |
| [mutation-testing-report-liberado-commands.md](mutation-testing/mutation-testing-report-liberado-commands.md) | Liberado-commands crate detail (campaign commit `72e4ab44`) |
| [mutation-testing-report-main-agent.md](mutation-testing/mutation-testing-report-main-agent.md) | Main-agent crate detail (updated 2026-08-23) |
| [mutation-testing-report-markdown.md](mutation-testing/mutation-testing-report-markdown.md) | Markdown crate detail (ledger rows at `0e14ecc` and `c23c9ca7`) |
| [mutation-testing-report-mcp.md](mutation-testing/mutation-testing-report-mcp.md) | Mcp crate detail (recorded 2026-08-25) |
| [mutation-testing-report-sysmap.md](mutation-testing/mutation-testing-report-sysmap.md) | Sysmap crate detail (campaign commit `1a452e09`) |
| [mutation-testing-report-sysmap-gui.md](mutation-testing/mutation-testing-report-sysmap-gui.md) | Sysmap-gui crate detail (campaign commit `1a452e09`) |
| [mutation-testing-report-telegram-approvals.md](mutation-testing/mutation-testing-report-telegram-approvals.md) | Telegram-approvals crate detail (recorded 2026-08-24) |
| [mutation-testing-report-test-support.md](mutation-testing/mutation-testing-report-test-support.md) | Test-support crate detail (campaign commit `1a452e09`) |
| [mutation-testing-report-theme.md](mutation-testing/mutation-testing-report-theme.md) | Theme crate detail (recorded 2026-08-24) |

**Naming notes:** `mutation-testing-report-v2.md` was renamed to `mutation-testing-report-dispatcher.md`. It covers only the dispatcher crate, not a second version of the overall mutation-testing program. `mutation-testing-report.md` — the original first-pass report — was renamed `mutation-testing-report-phase0-2026-07-23.md` to distinguish it from the aggregate plan.

**Related:** [coverage-gaps.md](coverage-gaps.md) (historical 2026-07-29 snapshot) and
[coverage-gaps-coding-features-2026-08-06.md](coverage-gaps-coding-features-2026-08-06.md)
(historical 2026-08-06 feature-branch snapshot) for dated llvm-cov evidence;
[mock-harness-scope.md](../future-work/archive/mock-harness-scope.md) for the historical test
infrastructure design; [property-testing-plan.md](../future-work/archive/property-testing-plan.md)
for the historical 2026-07-31 property-testing plan (live `proptest` usage is in crate test
modules and `Cargo.toml`); [docs/README.md](../README.md) for the full docs map.
