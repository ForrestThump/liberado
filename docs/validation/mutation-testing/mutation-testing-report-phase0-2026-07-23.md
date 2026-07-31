# Mutation Testing Report

Generated 2026-07-23 using `cargo-mutants 27.1.0`, updated after fixes.

Tool: `cargo mutants --package <crate> --in-place --cap-lints true`

## Post-Fix Results

| Crate | Mutants | Missed | Caught | Unviable | Baseline | Notes |
|---|---|---|---|---|---|---|
| **conversation-store** | 1 | 0 | 0 | 1 | OK | |
| **test-support** | 10 | 2 | 4 | 4 | OK | 2 false positives (`Vec::new()` ≡ `vec![]`) |
| **orchestrator** | 40 | 1 | 24 | 15 | OK | 1 false positive (`Vec::new()` ≡ `vec![]`) |
| **telegram-approvals** | 42 | 0 | 33 | 9 | OK | Full coverage of all actions, scopes, revision, env, run loop |
| **vault** | 54 | 0 | 12 | 42 | OK | |
| **session-store** | 113 | 0 | 39 | 70 | OK | Lineage propagation tested |
| **mcp** | 123 | 33 | 59 | 31 | OK | 7 caught; 33 remain in pool/live_runtime |
| **common** | 196 | — | — | — | **OK** | 2 test failures were cargo-mutants corruptions, now fixed |
| **executor** | 178 | — | — | — | **OK** | 3 test failures were cargo-mutants corruptions, now fixed |
| **provider** | 139 | — | — | — | **OK** | cargo-mutants corruption fixed; tests now pass |
| **coder-agent** | 330 | — | — | — | **FAILED** | Doctest cannot find crate `liberado_coder_tools` |
| **Totals** (10 crates testable) | |

## Pre-existing Issue Blocking Mutation Testing

- **coder-agent** (`crates/coder-agent/`): doctest cannot find crate `liberado_coder_tools` — pre-existing, not caused by mutation testing.

## Crates Unblocked

The following crates were initially blocked by test failures that turned out to be `cargo-mutants --in-place` corruptions left in the source tree. After restoring the original code, all tests pass:

| Crate | Corrupted Code | Fix |
|---|---|---|
| **common** | `WriteClass::allows_direct_agent_write()` body replaced with `false` | Restored `matches!(self, Self::AgentWritable \| Self::Shared)` |
| **executor** | `preview()` guard `<= MAX` replaced with `> MAX` (inverted truncation) | Restored `<=` |
| **provider** | `!used.insert(...)` — `!` deleted from `while` loop in `build_tool_name_map` | Restored `!` |

## Remaining Missed Mutants

### mcp (33 missed)

Concentrated in files requiring complex test infrastructure:

| File | Missed | Key untested areas |
|---|---|---|
| `src/pool.rs` | 20 | `ConnectionPool::reap_idle`, `spawn_reaper`, `invalidate`, `try_checkout` boundary, `AsToolRuntime`/`PermittedRuntime` delegation |
| `src/live_runtime.rs` | 8 | `LiveRegistryRuntime::sorted_names`, `refresh_sync`, `catalog`/`invoke` delegation (entire file untested) |
| `src/factory.rs` | 2 | `replace_connectors`, `publish_healthy` |
| `src/lib.rs` | 3 | `rebind_provenance`, `connection_is_dead` on `TurbomcpRuntime` |

These require mock servers, clock injection, or connection pool lifecycle tests.

### False Positives (structurally uncatchable)

| Crate | Location | Mutation | Reason |
|---|---|---|---|
| **test-support** | `NoopRuntime::catalog` | `Vec::new()` → `vec![]` | Semantically identical — same empty vector |
| **test-support** | `InvocationRecordingRuntime::catalog` | `Vec::new()` → `vec![]` | Same |
| **orchestrator** | `NoMcpRuntime::catalog` | `Vec::new()` → `vec![]` | Same |

## Tests Added

| Crate | File | Tests Added |
|---|---|---|
| **vault** | `src/lib.rs` | `next_event_yields_sent_event`, `next_event_returns_none_after_sender_drops` |
| **session-store** | `tests/conversation_lens.rs` | `create_stores_parent_conversation_lineage`, `create_stores_spawned_by_lineage` |
| **test-support** | `src/lib.rs` | `noop_catalog_is_empty`, `noop_invoke_returns_ok`, `recording_catalog_is_empty`, `recording_invoke_stores_call_and_returns_ok` |
| **orchestrator** | `src/lib.rs` | `terminal_summary_failed_outcome_maps_to_failed_terminal_kind`, `terminal_summary_partial_success_prefixes_the_summary`, `deferred_flag_of_reads_the_atomic`, `no_mcp_runtime_catalog_is_empty`, `no_mcp_runtime_invoke_returns_error` |
| **orchestrator** | `tests/orchestrate.rs` | `execute_approved_tool_calls_dedup_mcp_names`, `execute_approved_tool_calls_all_failed_is_failed_outcome`, `execute_approved_tool_calls_partial_failure_is_partial_outcome` |
| **telegram-approvals** | `src/lib.rs` | `handle_action_approve/reject/revise/deny/once/session/everywhere` (7 tests), `ack_calls_channel_acknowledge`, `handle_event_dispatches_actions`, `handle_event_ignores_bot_messages`, `handle_event_with_message_ref_is_forwarded`, `handle_message_revision_reply_is_routed`, `handle_message_non_revision_ignores_when_no_chat_surface`, `handle_message_empty_text_is_ignored`, `handle_message_slash_commands`, `handle_message_help_slash_command`, `begin_revision_records_prompt_stem_mapping`, `from_env_respects_env_vars`, `set_permission_scope_refuses_non_pending_proposal`, `run_registers_commands_and_processes_events` |
| **mcp** | `src/lib.rs` | `arguments_to_map_with_object`, `arguments_to_map_with_non_object`, `arguments_to_map_with_empty_object` |
| **mcp** | `src/multi.rs` | `is_empty_false_when_runtimes_registered`, `len_reflects_registered_count` |
| **mcp** | `src/factory.rs` | `is_empty_true_when_no_connectors`, `is_empty_false_and_len_reflects_count` |

## Summary

- **Before fixes:** 7 crates testable, 83 missed mutants across them. 4 crates blocked.
- **After fixes:** 10 crates testable. 3 false positives + 33 mcp remaining. 3 crates unblocked (cargo-mutants corruption fixed). 1 crate blocked (coder-agent doctest).
- **3 cargo-mutants corruptions found and fixed** in `common`, `executor`, `provider` — caused by interrupted `--in-place` mutation runs.
