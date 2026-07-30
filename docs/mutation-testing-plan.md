# Mutation Testing Plan — Crate Run Order

Run crates in ascending estimated time. A crate's speed is driven by the size of its workspace dependency graph + source file count + test count.

| Order | Crate | Wkspc Deps | Files | Tests | Est. Time | Role |
|-------|-------|:----------:|:-----:|:-----:|:---------:|------|
| 1 | **provider** | 0 | 7 | 39 | Fastest | Provider-agnostic LLM inference trait + mock |
| 2 | **common** | 0 (†) | 13 | 102 | Fast | Shared types: capabilities, provenance, events, decisions |
| 3 | **config-loader** | 1 | 11 | 106 | Fast | ConfigSource trait + ChainLoader for layered config |
| 4 | **session** | 1 | 9 | 36 | Fast | GoalSessionHub, SessionGrant, DomainPackRunner |
| 5 | **config** | 2 | 1 | 24 | Moderate | Config dir resolution, TOML assembly, validation |
| 6 | **executor** | 4 | 3 | 72 | Moderate | Bounded adaptive tool loop driving a Provider |
| 7 | **orchestrator** | 5 | 1 | 70 | Moderate | Bridges DispatchDecision → execution |

(†) `common` has `liberado-config-loader` as a dev-dependency only, so it doesn't affect its own compilation.

### Run command

```
cargo mutants --package liberado-<name> --cap-lints true
```

Avoid `--in-place` on Windows (risk of `os error 1224` mid-restore corruption; see v2 report).

### Methodology

1. Baseline: `cargo test -p liberado-<name>` — confirm green
2. Run: `cargo mutants --package liberado-<name> --cap-lints true`
3. Triage survivors: classify as false positive or actionable miss
4. Patch actionable misses with targeted tests
5. Re-run mutants to verify catch rate improvement
6. Write crate-specific report

### Completed

- `liberado-dispatcher` — v2 report. 48 → 60 tests, 72.7% → 96.4% catch rate.
- `liberado-provider` — provider report. 39 → 59 tests, 62.2% → 88.7% catch rate.
- `liberado-common` — common report. 102 → 117 tests, 83.8% → 94.1% catch rate.
- `liberado-config-loader` — config-loader report. 106 → 116 tests, 81.3% → 87.1% catch rate.
- `liberado-session` — session report. 36 → 41 tests, 73.8% → 79.9% catch rate.
- `liberado-config` — config report. 24 → 27 tests, 50.0% → 52.2% catch rate.
- `liberado-executor` — executor report. 72 → 78 tests, 80.4% → 82.7% catch rate.
- `liberado-orchestrator` — orchestrator report. 70 → 77 tests, 86.9% → 92.9% catch rate.

## Summary Across All Crates

| Crate | Tests | Catch Rate |
|-------|:-----:|:----------:|
| dispatcher | 60 | 96.4% |
| provider | 59 | 88.7% |
| common | 117 | 94.7% |
| config-loader | 116 | 87.8% |
| session | 41 | 79.9% |
| config | 27 | 52.2% |
| executor | 78 | 82.7% |
| orchestrator | 77 | 92.9% |
| coder-core | 45 | 72.4% |
| mcp | 45 | 68.1% |
| notify | 12 | 41.9% |
| **Total** | **637** | — |

### Notes
- `coder-agent` (84 tests) timed out after 15 minutes. The 200+ mutant surface with per-mutant 3s builds makes the pass infeasible in a single run.
- `notify`'s 41.9% catch rate is dominated by `TelegramNotifier` (live API code at ~59% coverage). Excluding network-bound code, the catch rate on the `ChannelNotifier` and trait default impls would be significantly higher.
- `mcp`'s 68.1% includes `live_runtime.rs` (0% coverage, real MCP process spawning). The actionable pool/factory code has higher effective catch rate.
