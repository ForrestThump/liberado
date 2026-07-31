# Mutation Testing Report — `liberado-common`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 102 | 117 | **+15** |
| Mutants tested | 240 | 240 | — |
| Caught | 171 | 191 | **+20** |
| Missed | 33 | 12 | **−21** |
| Timeout | 0 | 1 | +1 |
| Unviable | 36 | 36 | — |
| Catch rate (of viable) | 83.8% | 94.1% | **+10.3pp** |

No real code bugs were found during triage — all survivors are test-coverage gaps or false positives.

## What Was Found and Fixed

### Tests Added (15)

| Test | File | Area |
|------|------|------|
| `grants_ask_human_is_false_without_ask_human` | `capability.rs` | CapabilitySet |
| `mcp_name_returns_some_for_execute_mcp_and_execute_tool` | `capability.rs` | Capability |
| `truncate_at_exact_limit_preserves_full_length` | `capability.rs` | Instruction scope |
| `context_marker_truncates_before_instruction_limit` | `capability.rs` | Instruction scope |
| `truncate_snaps_to_char_boundary_when_cutoff_is_mid_char` | `capability.rs` | Instruction scope |
| `consequence_catalog_returns_all_consequences` | `catalog.rs` | MCP catalog |
| `write_target_empty_segment_is_not_a_zone` | `catalog.rs` | Write target |
| `degraded_purge_sends_notification_on_expiry` | `catalog.rs` | MCP catalog |
| `depth_is_normal_true_for_normal` | `dispatch.rs` | Depth enum |
| `delivery_is_summarize_true_for_summarize` | `dispatch.rs` | Delivery enum |
| `dispatch_action_display_matches_label` | `dispatch.rs` | DispatchAction Display |
| `bare_tool_name_strips_prefix` | `dispatch.rs` | Tool name parsing |
| `from_str_rejects_unknown_zone` | `local_time.rs` | Timezone parsing |
| `set_status_updates_status_in_place` | `proposal.rs` | SignedProposal |
| `clear_empties_all_grants` | `session_grants.rs` | Session grants |

## Remaining Missed Mutants (12)

| Location | Mutant | Reason |
|----------|--------|--------|
| `capability.rs:273` | `>` → `>=` in snap-left guard | `is_char_boundary(0)` always true — functionally identical |
| `capability.rs:363` | `empty` → `Default::default()` | `empty()` calls `Self::default()` — identical by definition |
| `catalog.rs:299` | `<` → `==`/`<=` in TTL comparison | Time-boundary; can't guarantee `now - at == ttl` in a test |
| `dispatch.rs:161` | `Depth::label` → `""`/`"xyzzy"` | Static-string getter; value visible only by testing each variant's output |
| `dispatch.rs:222` | `Delivery::label` → `""`/`"xyzzy"` | Same |
| `model.rs:37` | `ReasoningLevel::as_str` → `""`/`"xyzzy"` | Same |
| `proposal.rs:381` | `ProposedAction::summary` → `""`/`"xyzzy"` | Same |

1 additional timeout (caught behaviorally): `-=` → `/=` in truncate_to_instruction snap loop (infinite loop).
