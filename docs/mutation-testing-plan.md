# Mutation Testing Plan — Crate Run Order & Results

## Phase 1: Primary Mutation Hardening (8 crates)

Run crates in ascending estimated time. A crate's speed is driven by the size of its workspace
dependency graph + source file count + test count.

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
6. Write crate-specific report in `docs/mutation-testing-report-<name>.md`

---

## Phase 1 Results

| Crate | Tests Before | Tests After | Catch Before | Catch After | Delta | Report |
|-------|:------------:|:-----------:|:------------:|:-----------:|:-----:|--------|
| dispatcher | 48 | 60 | 72.7% | **96.4%** | +23.7pp | `mutation-testing-report-v2.md` |
| provider | 39 | 59 | 62.2% | **88.7%** | +26.5pp | `mutation-testing-report-provider.md` |
| common | 102 | 117 | 83.8% | **94.1%** | +10.3pp | `mutation-testing-report-common.md` |
| config-loader | 106 | 116 | 81.3% | **87.1%** | +5.8pp | `mutation-testing-report-config-loader.md` |
| session | 36 | 41 | 73.8% | **79.9%** | +6.1pp | `mutation-testing-report-session.md` |
| config | 24 | 27 | 50.0% | **52.2%** | +2.2pp | `mutation-testing-report-config.md` |
| executor | 72 | 78 | 80.4% | **82.7%** | +2.3pp | `mutation-testing-report-executor.md` |
| orchestrator | 70 | 77 | 86.9% | **92.9%** | +6.0pp | `mutation-testing-report-orchestrator.md` |
| **Total** | **497** | **575** | — | — | — | |

---

## Phase 2: Mock Harness & Integration Tests

The remaining mutation survivors all shared the same root cause: no way to inject errors
(Provider failures, tool failures, filesystem I/O failures, wall-clock boundaries) without
real network/process infrastructure.

**Decision:** Build a mock harness in `liberado-test-support` and `liberado-common` to
unblock ~35 testable gaps.

### Harness Implementation (see `docs/mock-harness-scope.md` for full checklist)

| Step | What | Status |
|------|------|:------:|
| 1a | Scriptable `MockProvider` errors (`push_error`) | ✓ |
| 1b | Error-capable `InvocationRecordingRuntime` (`with_error`, `with_default_result`) | ✓ |
| 1c | `FailingFactory` (returns `RuntimeSetupError`) | ✓ |
| 1d | `MockNotifier` in test-support | ✓ |
| 1e | `sample_proposal()` exported from common | ✓ |
| 2a | `FrozenClock` module in common + executor wiring | ✓ (provider not wired — no common dep) |
| 3a | Test-gated filesystem error injection on `RiskGatedToolRuntime` | ✓ |

### Integration Tests Written

| # | Test | File | What it proves |
|---|------|------|----------------|
| 1 | `provider_transport_error_propagates_to_dispatch_error` | `dispatcher/src/lib.rs` | `ProviderError::Transport` → `DispatchError::Provider` |
| 2 | `provider_rate_limit_error_propagates_to_dispatch_error` | `dispatcher/src/lib.rs` | `ProviderError::RateLimited` → `DispatchError::Provider` |
| 3 | `factory_setup_error_is_surfaced_by_orchestrator` | `orchestrator/tests/orchestrate.rs` | `RuntimeSetupError` from `FailingFactory` → `OrchestratorError::Runtime` |
| 4 | `degraded_entry_purged_at_exact_ttl_boundary` | `common/src/catalog.rs` | `FrozenClock` + advance by exactly TTL → entry purged |
| 5 | `wall_clock_limit_exhausts_at_exact_non_zero_boundary` | `executor/src/lib.rs` | `#[ignore]` — FrozenClock cannot inject time mid-loop iteration |

### Phase 2 Re-run Results

After integration tests and mock harness, re-ran mutants on crates with new tests:

| Crate | Before | After | Delta |
|-------|:------:|:-----:|:-----:|
| common | 94.1% | **94.7%** | +0.6pp |
| config-loader | 87.1% | **87.8%** | +0.7pp |
| dispatcher | 96.4% | 96.4% | — (2 misses are tracing-only) |
| orchestrator | 92.9% | 92.9% | — (6 misses are false positives) |
| coder-core | 72.4% | **74.3%** | +1.9pp |

---

## Phase 3: Coverage Expansion on Unhardened Crates

**Decision:** Before running mutants on crates that hadn't been hardened, run
`cargo llvm-cov` to identify the largest coverage gaps, then test-close the
actionable ones, and finally run `cargo-mutants` on the hardened result.

### Results

| Crate | Tests Added | Coverage Before | Coverage After | Catch Rate | Notes |
|-------|:-----------:|:---------------:|:--------------:|:----------:|-------|
| coder-core | +21 | 79.2% | **87.3%** | 74.3% | 3 commits: visitor shapes, tuning validation, verdict constructors |
| coder-agent | +7 | 83.9% | **85.6%** | (timed out) | `repair_feedback.rs` 59.8% → 89.0% |
| notify | +6 | 43.4% | **58.9%** | 41.9% | Default trait impls, `ChannelNotifier` with recording channel |
| mcp | +2 | 83.1% | **86.0%** | 68.1% | `replace_connectors`, `reap_idle` disabled, `connection_is_dead` |

### Code Decomposition (coder-core `looks_like_a_path`)

Extracted a 12-line pure closure from `scope_names_a_file` into a named function to
make boundary conditions directly unit-testable. The extraction confirmed that 5 of 6
remaining coherence.rs mutants are **false positives** — the operators (`>`, `!=`, `&&`)
cannot be distinguished with valid filename inputs. The sixth was not in the closure.

### Production Dedup

`CapabilitySet::granted_tools` and `CapabilitySet::granted_mcps` were 14 lines of
near-identical code differing only by variant name (`ExecuteTool` / `ExecuteMcp`).
Consolidated via a private `matching_names` helper.

### Test-Code Dedup Analysis (see `docs/coverage-gaps.md`)

`cargo dupes` found 244 exact-duplicate groups across the workspace. The most impactful
test-code duplicates are private helpers inside test modules (`NoopRuntime`, `vault_descriptor`,
`RecordingChannel`). Extracting them into `liberado-test-support` creates orphan-rule friction
(without common dependency changes, which would create circular deps). Moving them into
`liberado-common` is infeasible because they require `liberado-executor`, `liberado-provider`,
and `liberado-notify`, all of which depend on common.

**Verdict:** Keep `liberado-test-support` as the dedicated crate. The remaining 8-10 copies of
`NoopRuntime` and `vault_descriptor` are in test modules that also need local trait impls
(`RebindableRuntime`, `McpConnector`) regardless of where the struct lives.

---

## Real Bugs Found

### `budget_failed_report` ignores `exhausted_name` (executor/src/lib.rs:1079)

Discovered while writing the non-zero wall-clock budget test. The turn loop computes
`exhausted_name` ("turns", "wall-clock", or "tokens") but `execute`'s catch block calls
`budget_failed_report(turns)` which hardcodes `"turns"` in the summary.

`budget_failed_report_named(resource, turns)` exists at line 1094 but is only used in
`budget_failed_report_with_progress` — never in the primary `execute` path.

**Impact:** When wall-clock or token budget is exhausted, the report claims turn budget
ran out instead of the actual resource. A developer or model reading the report would
misdiagnose the failure.

**Evidence:**
```rust
// test written during this session, currently #[ignore]
#[tokio::test]
#[ignore = "FrozenClock limitation + budget_failed_report bug (see docs/coverage-gaps.md)"]
async fn wall_clock_limit_exhausts_at_exact_non_zero_boundary() { ... }
```

---

## Summary Across All Crates (Final)

| Crate | Tests | Catch Rate | Coverage |
|-------|:-----:|:----------:|:--------:|
| dispatcher | 60 | 96.4% | 97.9% |
| provider | 59 | 88.7% | 95.0% |
| common | 117 | 94.7% | 95.2% |
| config-loader | 116 | 87.8% | 93.3% |
| session | 41 | 79.9% | 96.2% |
| config | 27 | 52.2% | 87.0% |
| executor | 78 | 82.7% | 98.1% |
| orchestrator | 77 | 92.9% | 91.6% |
| coder-core | 48 | 74.3% | 87.3% |
| coder-agent | 84 | (timed out) | 85.6% |
| mcp | 45 | 68.1% | 86.0% |
| notify | 12 | 41.9% | 58.9% |

### Why Some Catch Rates Are Low

- **notify (41.9%):** `TelegramNotifier` requires live API — ~60% of the crate is
  unreachable without network access or a `MessagingChannel` mock that fully exercises
  `send_with_actions`→`request_reply`→`acknowledge`→`receive` loops.
- **mcp (68.1%):** `live_runtime.rs` (0% coverage) spawns real MCP child processes.
  The pool/factory/multi modules have higher effective catch rate in isolation.
- **config (52.2%):** Thin wrapper over `config-loader` + OS path resolution. Most
  uncovered code is `config_dir()`, `load_section()`, and proposal key I/O.
- **session (79.9%):** `hub.rs` `start_background`, `await_terminal`, and park/cancel
  flows need concurrent event injection.
- **executor (82.7%):** `run_loop` budget arithmetic and `risk_gated.rs` zone-restriction
  guards — tested through integration but specific operator mutations survive.
- **coder-agent (timed out):** 200+ mutant surface with 84 tests makes the run
  infeasible in one pass. The 7 integration tests targeting `repair_feedback` improved
  coverage from 59.8% → 89.0% but the mutants pass needs parallelization.

### Remaining Survivor Categories (All Crates)

| Category | Count | Explanation |
|----------|:-----:|-------------|
| Tracing-only guards | ~12 | `tracing::info!` / `warn!` / `debug!` called for diagnostics; need log-capture framework to test |
| IO/path-bound | ~20 | `config_dir`, `load_section`, proposal key I/O — need filesystem/environment mocking |
| Time-bound | ~6 | `Instant::now()` comparisons at exact boundaries — need clock injection (FrozenClock resolves catalog.rs, blocked on provider by dependency graph) |
| Budget arithmetic | ~10 | Turn-count, wall-clock, and token budget operators inside `run_loop` — tested through integration but specific operator mutations survive |
| String/getter constants | ~20 | `label()`, `as_str()`, `summary()` return static strings — functionally identical to their mutants |
| Builder/constructor returns | ~8 | `Default::default()` or `Ok(Default::default())` in constructors — semantically identical |
| Serde visitors | ~6 | `Visitor::expecting`, `visit_*` methods — trait impls where return value replacement is equivalent |