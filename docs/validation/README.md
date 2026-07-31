# Validation

Correctness artifacts for the Liberado workspace: mutation-testing reports, coverage analysis, and test-infrastructure design.

**Summary document — start here:** [`mutation-testing-plan.md`](mutation-testing-plan.md). It holds the master plan and Phase 1–5 results across all 12 hardened crates (run order, methodology, catch rates, survivor triage, Phase 5 roadmap). The per-crate reports below are supporting detail.

| Doc | Role |
|-----|------|
| [mutation-testing-plan.md](mutation-testing-plan.md) | **Summary** — master plan + Phase 1–5 results across 12 crates |
| [coverage-gaps.md](coverage-gaps.md) | Known uncovered code paths — IO/network/clock-gated, tracing-only, defaults |
| [mock-harness-scope.md](mock-harness-scope.md) | Test-infrastructure design — scriptable error mocks, FrozenClock, filesystem stubs |
| [mutation-testing-report.md](mutation-testing-report.md) | Original first-pass run across 10 crates (2026-07-23) |
| [mutation-testing-report-dispatcher.md](mutation-testing-report-dispatcher.md) | Dispatcher crate detail — 96.4% catch rate; dispatcher-only, not a true "v2" of the program |
| [mutation-testing-report-provider.md](mutation-testing-report-provider.md) | Provider crate detail (Phase 1) |
| [mutation-testing-report-common.md](mutation-testing-report-common.md) | Common crate detail (Phase 1) |
| [mutation-testing-report-config-loader.md](mutation-testing-report-config-loader.md) | Config-loader crate detail (Phase 1) |
| [mutation-testing-report-session.md](mutation-testing-report-session.md) | Session crate detail (Phase 1) |
| [mutation-testing-report-config.md](mutation-testing-report-config.md) | Config crate detail (Phase 1) |
| [mutation-testing-report-executor.md](mutation-testing-report-executor.md) | Executor crate detail (Phase 1) |
| [mutation-testing-report-orchestrator.md](mutation-testing-report-orchestrator.md) | Orchestrator crate detail (Phase 1) |
| [mutation-testing-report-coder-sandbox.md](mutation-testing-report-coder-sandbox.md) | Coder-sandbox crate detail (Phase 4) |
| [mutation-testing-report-coder-tools.md](mutation-testing-report-coder-tools.md) | Coder-tools crate detail (Phase 4) |
| [mutation-testing-report-daemon.md](mutation-testing-report-daemon.md) | Daemon crate detail (Phase 4) |
| [mutation-testing-report-server.md](mutation-testing-report-server.md) | Server crate detail (Phase 4) |
| [mutation-testing-report-coder-agent.md](mutation-testing-report-coder-agent.md) | Coder-agent crate detail (Phase 4) |

**Naming note:** `mutation-testing-report-v2.md` was renamed to `mutation-testing-report-dispatcher.md`. It covers only the dispatcher crate, not a second version of the overall mutation-testing program.

**Related:** [coverage-gaps.md](coverage-gaps.md) for known uncovered code paths; [mock-harness-scope.md](mock-harness-scope.md) for test infrastructure design; [docs/README.md](../README.md) for the full docs map.
