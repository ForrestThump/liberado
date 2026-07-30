# Integration Test Expansion — Plan

Generated 2026-07-29. Uses the mock harness built in `docs/mock-harness-scope.md`.

## Prerequisite Survey

### Already covered (do not rewrite)
| Test | File | What it asserts |
|------|------|-----------------|
| `genuine_provider_failure_propagates` | `dispatcher/src/lib.rs:1507` | Empty mock → `MockExhausted` → `DispatchError::Provider` |
| `complete_json_propagates_provider_error` | `provider/tests/coverage.rs` | `MockExhausted` propagates through `complete_json` |
| `complete_json_retries_an_unusable_reply_once` | `provider/tests/coverage.rs` | Bad JSON → `Decode` → retry |
| `complete_json_maps_empty_content_to_empty_response` | `provider/tests/coverage.rs` | Empty content mapping |
| `a_proposal_write_failure_is_a_real_error_not_a_silent_ok` | `executor/src/risk_gated.rs:1015` | Filesystem failure → Err (covers phases 3a both paths) |
| `downgrade_with_a_confirmed_notify_records_out_of_band_deferral` | `executor/src/risk_gated.rs:894` | MockNotifier succeeds |
| `downgrade_whose_notify_failed_records_no_deferral` | `executor/src/risk_gated.rs` | MockNotifier fails |
| `recording_invoke_with_per_tool_error` | `test-support/src/lib.rs` | Per-tool error injection works |
| `half_open_ttl_reincludes_peer_in_routing` | `common/src/catalog.rs` | Degraded → purge → re-included |
| `wall_clock_limit_exhausts_before_the_first_turn_when_set_to_zero` | `executor/src/lib.rs` | Zero budget exhausts immediately |

### Truly missing (write these)
| # | Test | File | Harness feature |
|---|------|------|-----------------|
| 1 | `provider_transport_error_propagates_to_dispatch_error` | `dispatcher/src/lib.rs` | `push_error(Transport(...))` |
| 2 | `provider_rate_limit_error_propagates_to_dispatch_error` | `dispatcher/src/lib.rs` | `push_error(RateLimited)` |
| 3 | `factory_setup_error_is_surfaced_by_orchestrator` | `orchestrator/tests/orchestrate.rs` | `FailingFactory` |
| 4 | `degraded_entry_purged_at_exact_ttl_boundary` | `common/src/catalog.rs` | `test_freeze_at` + `test_advance` |
| 5 | `wall_clock_limit_exhausted_mid_run` | `executor/src/lib.rs` | `test_freeze_at` + `test_advance` |

## Implementation Order

Each step: write test → `cargo test -p <pkg>` → `cargo fmt --all` → `cargo clippy --all -- -D warnings` → commit.

### Step 1: Dispatcher provider error tests (tests 1-2)
**File:** `crates/dispatcher/src/lib.rs`, inside the existing `mod tests`
**Approach:** Use `MockProvider::new("m")` then `.push_error(ProviderError::Transport(...))` before dispatch. Assert `DispatchError::Provider(err)` and check the error variant.

### Step 2: Orchestrator factory error test (test 3)
**File:** `crates/orchestrator/tests/orchestrate.rs`
**Approach:** Create orchestrator with `FailingFactory::new("MCP launch failed")`, attempt `execute_direct`. Assert the error propagates.

### Step 3: Catalog TTL boundary test (test 4)
**File:** `crates/common/src/catalog.rs`, inside `mod tests`
**Approach:** `test_freeze_at` → mark degraded with instant → advance clock by exactly TTL → `is_degraded` → assert false (purged).

### Step 4: Executor wall-clock budget test (test 5)
**File:** `crates/executor/src/lib.rs`, inside `mod tests`
**Approach:** Create executor with wall-clock budget, `test_freeze_at` start, advance past limit, run one turn. Assert `BudgetExceeded`.
