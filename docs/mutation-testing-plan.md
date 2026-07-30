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

---

## Phase 4: Remaining Crate Hardening (5 crates)

Ordered by estimated build cost (workspace deps × source files × test count).

### Pre-check: cargo-mutants timeout flags

```
cargo mutants --help | rg timeout
```

Output shows:
- `--timeout N` — **multiplier** (relative to baseline test time), not absolute seconds
- `--minimum-test-timeout N` — absolute floor in seconds (lower bound on auto-set time)
- `--build-timeout N` / `--build-timeout-multiplier N` — same for build phase

**Recommendation:** start with `--timeout 3.0` (3× baseline) and
`--minimum-test-timeout 30` (no test gets less than 30s). Raise the floor for
crates with heavier integration tests (daemon 60s, server 90s) — see table below.
This ensures short tests aren't killed by a tight multiplier while hung tests
time out within a reasonable bound.

The overall process has no built-in timeout; it runs until all mutants are evaluated.
To let it run for hours, ensure the shell/terminal session won't time out (no `timeout`
wrapper, or explicitly set a large value).

| Crate | Src Files | Wkspc Deps | Tests | Mutants (est.) | Per-Mutant Build | Timeout Flags |
|-------|:---------:|:----------:|:-----:|:--------------:|:----------------:|---------------|
| **coder-sandbox** | 1 | 1 | 9 | ~30–50 | ~10–20s | `--timeout 3.0 --minimum-test-timeout 30` |
| **coder-tools** | 1 | 4 | 10 | ~40–80 | ~15–30s | `--timeout 3.0 --minimum-test-timeout 30` |
| **daemon** | 8 | 13 | 39 | ~80–150 | ~30–60s | `--timeout 4.0 --minimum-test-timeout 60` |
| **server** | 14 | 21 | 63* | ~100–200 | ~45–90s | `--timeout 5.0 --minimum-test-timeout 90` |
| **coder-agent** | 16 | 8 | 53† | 200+ | ~20–60s | `--timeout 3.0 --minimum-test-timeout 30` |

\* Server tests include 16 `t1_conformance` + 12 hooks + 12 goals + 7 chat + 5 cron-delivery + 4 sticky + 5 others
† coder-agent includes 30 unit tests + 23 integration tests (`completion_gate_e2e` 12, `mock_intake_e2e` 8, `live_scaffold` 3)

### Checklist

Each step includes the exact command with crate-specific timeout flags. Avoid `--in-place` on Windows.

#### 1. coder-sandbox — workspace isolation primitives

| Step | What | Status |
|------|------|:------:|
| 1a | Baseline: `cargo test -p liberado-coder-sandbox` | ✓ |
| 1b | Run: `cargo mutants -p liberado-coder-sandbox --cap-lints true --timeout 3.0 --minimum-test-timeout 30` | ✓ |
| 1c | Triage survivors | ✓ |
| 1d | Patch actionable misses | ✓ |
| 1e | Re-run to verify catch rate | ✓ |
| 1f | Report: `docs/mutation-testing-report-coder-sandbox.md` | ⬜ |

**What's inside** (`crates/coder-sandbox/src/lib.rs`): `HostWorkspace`, `DockerWorkspace`,
`PathPolicy`, shadow-git checkpoint scaffold, workspace-relative path validation.

**Key risks**: None — single file, fast tests, no network or I/O beyond already-mocked filesystem calls.

#### 2. coder-tools — the 10 coding tools

| Step | What | Status |
|------|------|:------:|
| 2a | Baseline: `cargo test -p liberado-coder-tools` | ⬜ |
| 2b | Run: `cargo mutants -p liberado-coder-tools --cap-lints true --timeout 3.0 --minimum-test-timeout 30` | ⬜ |
| 2c | Triage survivors | ⬜ |
| 2d | Patch actionable misses | ⬜ |
| 2e | Re-run to verify catch rate | ⬜ |
| 2f | Report: `docs/mutation-testing-report-coder-tools.md` | ⬜ |

**What's inside** (`crates/coder-tools/src/lib.rs`): `list_files`, `search_text`, `read_file`,
`write_file`, `edit_file`, `apply_patch`, `git_status`, `git_diff`, `run_command`, `validate`.

**Key risks**: `run_command` and `git_diff` spawn child processes. Mutants that break the
subprocess path may hang. The `--timeout 3.0 --minimum-test-timeout 30` flags catch
hangs. The existing 10 tests (`tool_schemas_are_non_empty`, path validation, etc.) are fast.

#### 3. daemon — composition root (cron + proposals + sessions)

| Step | What | Status |
|------|------|:------:|
| 3a | Baseline: `cargo test -p liberado-daemon` | ⬜ |
| 3b | Run: `cargo mutants -p liberado-daemon --cap-lints true --timeout 4.0 --minimum-test-timeout 60` | ⬜ |
| 3c | Triage survivors | ⬜ |
| 3d | Patch actionable misses | ⬜ |
| 3e | Re-run to verify catch rate | ⬜ |
| 3f | Report: `docs/mutation-testing-report-daemon.md` | ⬜ |

**What's inside** (`crates/daemon/src/`): `lib.rs` (29 tests — daemon bootstrap, pool routing,
schedule wiring), `proposals.rs` (7 tests — expiry reaper, proposal lifecycle),
`debounce.rs` (3 tests). 39 tests total across 8 source files.

**Key risks**: The daemon test in `lib.rs` constructs a full daemon with mock providers and
runs schedules. Mutants in scheduling/dispatch logic could cause deadlocks. 13 workspace
deps means moderate compile time per mutant. `--timeout 4.0 --minimum-test-timeout 60`
gives slower tests enough room while capping hung ones.

**Why it matters**: The daemon is the composition root where config, policy, cron, and
dispatcher meet. Bugs here are the Class 6 kind — two things that should agree don't —
and they manifest as silent failures, not panic stacks.

#### 4. server — HTTP surface + T1 conformance + hooks + goals

| Step | What | Status |
|------|------|:------:|
| 4a | Baseline: `cargo test -p liberado-server` | ⬜ |
| 4b | Run: `cargo mutants -p liberado-server --cap-lints true --timeout 5.0 --minimum-test-timeout 90` | ⬜ |
| 4c | Triage survivors | ⬜ |
| 4d | Patch actionable misses | ⬜ |
| 4e | Re-run to verify catch rate | ⬜ |
| 4f | Report: `docs/mutation-testing-report-server.md` | ⬜ |

**What's inside** (`crates/server/src/`): `t1_conformance.rs` (16 tests — Tier 1 live
conformance L1–L10), `api/goals.rs` (12), `api/chat.rs` (7), `hooks.rs` (12),
`cron_delivery.rs` (5), `sticky.rs` (4). 63 tests across 14 source files.

**Key risks**: `t1_conformance` spawns a real `liberado-server` on a temp port with temp
dirs and a `MockProvider`. These are the heaviest tests in the workspace — they exercise
the full HTTP→dispatch→execute path. Mutants that break the HTTP surface may produce
misleading survivors (test setup fails, mutant counted as "caught" only because nothing
ran — not because the test correctly asserted the wrong result).

21 workspace deps means the longest per-mutant compile time in this phase. `--timeout 5.0
--minimum-test-timeout 90` accommodates slow T1 tests. Estimate: 100–200 mutants ×
45–90s = 75 min to 5 hours total. Worth running overnight.

#### 5. coder-agent — intake → planner → worker → verifier → critic pipeline

**Retry of the timed-out Phase 3 run.** The 200+ mutant surface plus 53 tests (some
integration-grade) made the default run infeasible. Strategy: short per-mutant timeout
so hung tests don't accumulate, but let the process run for hours to cover the full surface.

| Step | What | Status |
|------|------|:------:|
| 5a | Baseline: `cargo test -p liberado-coder-agent` | ⬜ |
| 5b | Run: `cargo mutants -p liberado-coder-agent --cap-lints true --timeout 3.0 --minimum-test-timeout 30` | ⬜ |
| 5c | If any test hangs consistently, `#[ignore]` it and re-run | ⬜ |
| 5d | Triage survivors | ⬜ |
| 5e | Patch actionable misses | ⬜ |
| 5f | Re-run to verify catch rate | ⬜ |
| 5g | Report: `docs/mutation-testing-report-coder-agent.md` | ⬜ |

**What's inside** (`crates/coder-agent/src/`): 16 source files — `lib.rs`, `completion_gate.rs`
(5 tests), `gates.rs` (2), `planner.rs` (1), `intake_session.rs` (1), `repair_feedback.rs`
(10), `progress.rs` (5), `session_pack/tests.rs` (4), plus 23 integration tests in
`tests/`. The Phase 3 coverage expansion improved `repair_feedback.rs` from 59.8%→89.0%
but mutants were not re-run.

**Key risks**:
- `completion_gate_e2e.rs` (12 integration tests) exercises the full gatekeeper+quorum
  flow with mock reviewers. Mutants in gate quorum logic may produce infinite retry loops.
- `mock_intake_e2e.rs` (8 tests) runs the intake→contract pipeline end-to-end.
- `live_scaffold.rs` (3 tests) is `#[ignore]`d — shouldn't affect the run but verify.
- `--timeout 3.0 --minimum-test-timeout 30` gives each mutant 3× its baseline runtime with a
  30-second floor. A hung test (e.g., budget loop never exits) will time out at ~30–60s.
  Timeout ≠ caught, so a high miss rate from timeouts needs investigation, but it's
  better than the process dying after 15 minutes with zero results.
- Ensure the shell has no external timeout: `$env:TIMEOUT = $null`.

**Estimated runtime**: 200 mutants. If most tests complete near their baseline (0.5–2s),
each mutant takes ~30s floor + <5s compile × 200 = ~100 minutes. If integration tests
routinely hit the 3.0 multiplier (~15s actual), runtime climbs to ~90 minutes.
Expect **1.5–3 hours**. Run overnight if convenient; run during the day and check
periodically. Do NOT wrap in `Start-Process -NoNewWindow` or `timeout` — let the cargo
process own its lifetime.

### Phase 4 Results (to fill)

| Crate | Tests Before | Tests After | Catch Before | Catch After | Delta | Report |
|-------|:------------:|:-----------:|:------------:|:-----------:|:-----:|--------|
| coder-sandbox | 8 | 13 | —% | **87%** | — | `mutation-testing-report-coder-sandbox.md` |
| coder-tools | — | — | —% | —% | — | `mutation-testing-report-coder-tools.md` |
| daemon | — | — | —% | —% | — | `mutation-testing-report-daemon.md` |
| server | — | — | —% | —% | — | `mutation-testing-report-server.md` |
| coder-agent | — | — | —% | —% | — | `mutation-testing-report-coder-agent.md` |

### Survivor Triage Guide (repeated from Phase 1 for reference)

| Category | Action |
|----------|--------|
| Tracing/diagnostic (`tracing::info!`, `warn!`, `debug!`) | Ignore — need log-capture framework; zero behavioral impact |
| String/getter constants (`label()`, `as_str()`) | Ignore — different wording, same semantics; false positive |
| Builder/constructor defaults | Ignore — `Ok(Default::default())` identical to `Ok(Foo::default())` |
| IO/path — `config_dir`, `read_to_string`, `create_dir_all` | Document as infrastructure-gated; file under `coverage-gaps.md` |
| Budget/loop arithmetic | Test with `FrozenClock` + `Budget::new(N).with_wall_clock(D)` |
| Gate/completion logic | Test with mock reviewers + quorum math assertions |
| Tool-call routing/policy | Test with `InvocationRecordingRuntime` asserting which tools fired |
| Auth/zone-check guards | **High priority** — capability narrowing must be caught; add positive+negative arms |