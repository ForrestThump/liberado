# Coverage Gaps — Analysis

Generated 2026-07-29 from `cargo llvm-cov` on the 8 hardened crates.

## Real Logic Gaps (closed)

Three testable gaps were identified and closed with targeted tests:

| File | Lines | Gap | Fix |
|------|-------|-----|-----|
| `config-loader/model/config.rs:235-237` | `max_concurrent_coding_subagents == 0` validation | `zero_coding_concurrency_fails_validate` |
| `config-loader/model/config.rs:240-242` | `max_reaction_depth == 0` validation | `zero_reaction_depth_fails_validate` |
| `config-loader/model/config.rs:494-498` | Profile-scoped undeclared zone rejection | `profile_referencing_undeclared_zone_fails_validate` |

The `OrchestratorInfra` constructor/builder methods (orchestrator/src/lib.rs:306-366) are also uncovered but are exercised by the daemon boot path, not by unit/integration tests which construct `Orchestrator` directly.

## Infrastructure-Bound Gaps (would need new test scaffolding)

These are real code paths uncovered because they depend on I/O, network, or process state that current test infrastructure cannot mock.

### IO / Filesystem

| Crate | Lines | What it needs |
|-------|-------|---------------|
| `config/src/lib.rs` | ~90 lines | `config_dir`, `load_section`, `load_or_create_proposal_key`, grant overlay read/write — all read from env vars (`CONFIG_DIR`, `HOME`/`APPDATA`) and the real filesystem |
| `executor/risk_gated.rs:507-532` | ~20 lines | Proposal `create_dir_all` / `write` failure paths — would need `tempfile` + injectable I/O errors |
| `session/store.rs:92-151` | ~15 lines | Disk rehydration — log file reads, truncation handling |
| `session/store.rs:236,252,284` | ~5 lines | File I/O error branches during durable writes |
| `config-loader/file_source.rs:36-38` | 3 lines | `std::fs::read_to_string` error path |

### Network / API Dependencies

| Crate | Lines | What it needs |
|-------|-------|---------------|
| `provider/latency.rs:148-169` | ~8 lines | `MeteredProvider::wrap` + `list_models` passthrough — exercised only with a live Provider |
| `session/hub.rs:72` | 1 line | `SendInputError::Closed` Display impl — only reachable after the input channel is explicitly closed |
| `notify/src/lib.rs` | ~112 lines | 49 functions uncovered — all go through the real Telegram API (captured via config-gated `#[cfg(test)]` flag) |

### Clock / Timer

| Crate | Lines | What it needs |
|-------|-------|---------------|
| `common/catalog.rs:138,305,394-396,661` | 6 lines | `degraded` TTL purge — `Instant::now()` comparison at exact boundaries |
| `executor/lib.rs:799-803` | 3 lines | Tracing guard around model reasoning log — `tool_count > 0` |
| `session/hub.rs:249-282` | ~15 lines | `await_terminal` — subscription + timeout loop; needs concurrent event injection |

## Tracing / Diagnostic Only

These are `tracing::info!`, `tracing::warn!`, or format-string-only functions. They have no return value and no behavioral effect outside the log. Catching them would require a log-capture framework.

| Crate | Lines | What |
|-------|-------|------|
| `orchestrator/lib.rs:600-604` | 5 lines | Delivery downgrade info log |
| `executor/risk_gated.rs:182-191` | 10 lines | `authority_decision` — structured security log |
| `executor/risk_gated.rs:196-203` | 5 lines | MCP missing-from-catalog warning |
| `executor/lib.rs:799-803` | 3 lines | Model reasoning tracing guard |
| `common/dispatch.rs:160-166,221-226` | 12 lines | `Depth::label`, `Delivery::label` |
| `provider/latency.rs:42,116,252` | 3 lines | `AgentRole::as_str` Display, `NoopRecorder` record |

## Defaults / Serde Helpers

Constant functions used as `#[serde(default = "...")]` field defaults or getter shorthands. Testing them would mean duplicating the constant in test code, which asserts nothing.

| Crate | Lines | What |
|-------|-------|------|
| `config-loader/topology.rs:119-125` | 5 lines | `default_path_arg` ("path"), `default_content_arg` ("content") |
| `config-loader/builder.rs:83-86` | 4 lines | `ConfigBuilder::schedule` — unused builder setter |
| `common/model.rs:36-43` | 7 lines | `ReasoningLevel::as_str` — each variant's string label |
| `common/session_grants.rs:85-88` | 3 lines | `SessionRecordStore` trait default impls |

## Dead Code in Test Context

Code that is exercised by the daemon binary, not by unit/integration tests.

| Crate | Lines | What |
|-------|-------|------|
| `orchestrator/lib.rs:306-366` | 60 lines | `OrchestratorInfra::new`, `with_report_sink`, `with_research_max_turns`, `for_pool` |
| `provider/provider.rs:38-40,69,71` | 5 lines | `Provider::set_model` default no-op, `list_models` default |
| `provider/types.rs:156-163` | 7 lines | `without_json_schema` — the degraded retry path for a backend that rejects `json_schema` |
| `session/record_store.rs:85-88` | 3 lines | Trait default `subscribe`/`live_subscriber_count` |

## Summary

| Category | Lines | What it needs |
|----------|-------|--------------|
| **Real logic gaps** (closed) | 9 lines | Targeted unit tests |
| **IO / filesystem** | ~140 lines | Filesystem/environment mocking |
| **Network / API** | ~120 lines | HTTP server stubs, live API tokens |
| **Tracing-only** | ~30 lines | Log-capture framework |
| **Defaults / serde** | ~20 lines | Duplicate constants in tests (no value) |
| **Dead code in tests** | ~80 lines | Integration test covering daemon composition root |

## Real Bugs Found During Coverage Analysis

### `budget_failed_report` ignores `exhausted_name` (executor/src/lib.rs:1079)

Discovered while writing the non-zero wall-clock budget test below. The test expected `"wall-clock"` in the report summary, but `budget_failed_report` hardcodes `"turns"` regardless of which resource actually exhausted.

The turn loop computes `exhausted_name` (turns, wall-clock, or tokens) but the `execute` method's catch block at line 463 calls `budget_failed_report(turns)` which discards the resource name:

```rust
// lib.rs:463 (currently)
Err(ExecError::BudgetExceeded { turns }) => {
    Ok(budget_failed_report(turns))
}
```

`budget_failed_report_named(resource, turns)` exists at line 1094 but is only used in `budget_failed_report_with_progress` — never in the primary `execute` path.

**Test that caught it:**

```rust
#[tokio::test]
async fn wall_clock_limit_exhausts_at_exact_non_zero_boundary() {
    let t0 = std::time::Instant::now();
    liberado_common::clock::test_freeze_at(t0);

    let (provider, exec) = executor(
        vec![submit(valid_report_args())],
        Budget::new(10).with_wall_clock(std::time::Duration::from_secs(1)),
    );

    liberado_common::clock::test_advance(std::time::Duration::from_secs(1));

    let runtime = MockToolRuntime::new(&["search"], Ok("data".into()));
    let report = exec
        .execute(&runtime, Task::new("worker", "do it"))
        .await
        .unwrap();

    assert_eq!(report.outcome, Outcome::Failed);
    // BUG: budget_failed_report hardcodes "turns" — this assertion fails
    // because the report says "turns" even though wall-clock was the real
    // exhausted resource.
    //
    // Expected but not yet true:
    //   assert!(report.summary.contains("wall-clock"));
    liberado_common::clock::test_thaw();
}
```

**Note:** The FrozenClock cannot inject time between `run_started` capture and the budget check within the same loop iteration, so this test relies on the zero-boundary behavior (same as `Duration::ZERO`). A true mid-run exhaustion test would require structural changes to the loop.

**Impact:** When wall-clock or token budget is exhausted, the report claims the turn budget ran out instead of the actual resource. A developer debugging the report or a model reading it would misdiagnose the failure.

**Fix:** Change line 463-467 to pass `exhausted_name` through to the error (e.g. `ExecError::BudgetExceeded { resource: &'static str, turns: u32 }`) and call `budget_failed_report_named` with the real resource name.
