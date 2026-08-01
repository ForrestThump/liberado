# Mutation Testing Report — `liberado-provider`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 39 | 59 | **+20** |
| Mutants tested | 183 | 183 | — |
| Caught | 61 | 86 | **+25** |
| Missed | 37 | 11 | **−26** |
| Timeout | 2 | 3 | +1 |
| Unviable | 83 | 83 | — |
| Catch rate (of viable) | 62.2% | 88.7% | **+26.5pp** |

## What Was Found and Fixed

### 1. `map_finish_reason` missing match arms (`openai_compat.rs:73-74`)

**Missed mutants:** Delete match arms `"length"` and `"content_filter"` — both would fall through to `FinishReason::Stop`.

Existing tests only exercised `"stop"` and `"tool_calls"` (via response parsing). The two remaining finish reasons were untested.

**Tests added (2):**
- `map_finish_reason_recognizes_length`
- `map_finish_reason_recognizes_content_filter`

### 2. `has_json_schema` untested (`types.rs:142`, `provider.rs:108`)

**Missed mutants:** Replace return with `true`/`false` on `has_json_schema()`. Also guards in `complete_json` that branch on it.

**Tests added (3):**
- `has_json_schema_is_false_for_text_format`
- `has_json_schema_is_true_for_constraining_schema`
- `has_json_schema_is_false_for_shapeless_schema`

### 3. `ToolAcc::into_invocation` edge cases (`openai_compat.rs:88-99`)

**Missed mutant:** Replace return with `None`.

**Tests added (3):**
- `into_invocation_returns_none_for_empty_name`
- `into_invocation_maps_name_through_reverse_map`
- `into_invocation_uses_raw_name_when_not_in_map`

### 4. `accumulate_tool_deltas` untested (`openai_compat.rs:103-125`)

**Missed mutants:** Whole function replaced with `()`, `<=` → `>`, `delete !` on id/name guards.

**Tests added (2):**
- `accumulate_tool_deltas_expands_slots_and_sets_fields`
- `accumulate_tool_deltas_does_not_overwrite_with_empty_id_or_name`

### 5. MockProvider `set_models` / `set_model` untested (`mock.rs:39`, `82-85`)

**Missed mutants:** `set_models` set to no-op, `set_model` set to no-op, `delete !` on empty-check guard.

**Tests added (2):**
- `set_models_is_returned_by_list_models`
- `set_model_rejects_empty_string`

### 6. MeteredProvider model forwarding (`latency.rs:160-164`)

**Missed mutants:** `model()` return replaced with empty/constant, `set_model` set to no-op.

**Test added (1):**
- `metered_provider_forwards_model_getter_and_setter`

### 7. `complete_stream` empty content (`provider.rs:56`)

**Missed mutant:** `delete !` on `!content.is_empty()`, causing empty content to emit a spurious token.

**Tests added (2):**
- `complete_stream_emits_no_tokens_for_empty_content`
- `complete_stream_emits_no_tokens_for_pure_tool_calls`

### 8. Stream TTFT recording (`latency.rs:226-229`)

**Missed mutants:** `ttft_ms.is_none()` guard forced `false`, `!recorded` guard forced `false`, `delete !` on `!recorded`.

**Test added (1):**
- `complete_stream_records_ttft_and_final_event`

### 9. `window_around` boundary conditions (`provider.rs:225-264`)

**Missed mutants:** Multi-line line-offset operator (`-` → `/`), ellipsis boundary operators (`>` → `==`/`<`/`>=` at 4 positions), snap-left direction (`-=` → `+=`).

**Tests added (4):**
- `window_line_offset_on_line_2_is_correct`
- `window_truncates_long_before_and_after`
- `window_omits_ellipsis_at_exactly_radius`
- `window_omits_tail_ellipsis_at_exactly_radius`

## Remaining Missed Mutants (11)

| Location | Mutant | Reason |
|----------|--------|--------|
| `latency.rs:120` | `now_ms → 0/1` | Wall-clock time; cannot assert `SystemTime::now()` deterministically |
| `latency.rs:226` | `ttft_ms.is_none() → true` | Single-token stream path produces same result — multi-token stream would distinguish |
| `latency.rs:229` | `!recorded → true` | Single-Done stream path produces same result — multi-Done stream would distinguish |
| `openai_compat.rs:60` | `+= → -= / *=` | Name collision suffix; 2-tool collision test doesn't distinguish these — 3+ tools needed to hit edge |
| `provider.rs:39` | `Provider::set_model → ()` | Default trait impl is already `let _ = model;` — functionally identical |
| `provider.rs:108` | `has_json_schema() → true/false` | Fallback guard in `complete_json`; needs scripted-error support in MockProvider to test |
| `provider.rs:242` | `>` → `>=` in snap-left loop | `byte_idx=0` always satisfies `is_char_boundary(0)` — short-circuits before body |
| `provider.rs:243` | `-=` → `+=` in snap-left loop | Only reachable when byte column lands mid-multi-byte character; hard to target precisely |

3 additional timeouts (caught behaviorally): `delete !` in `build_tool_name_map` (infinite loop), `<=` → `>` in `accumulate_tool_deltas`, `-=` → `/=` in `window_around` snap-left loop.

## Tests Added (20)

| Test | File | Area |
|------|------|------|
| `map_finish_reason_recognizes_length` | `openai_compat.rs` | Finish reason |
| `map_finish_reason_recognizes_content_filter` | `openai_compat.rs` | Finish reason |
| `into_invocation_returns_none_for_empty_name` | `openai_compat.rs` | Tool delta |
| `into_invocation_maps_name_through_reverse_map` | `openai_compat.rs` | Tool delta |
| `into_invocation_uses_raw_name_when_not_in_map` | `openai_compat.rs` | Tool delta |
| `accumulate_tool_deltas_expands_slots_and_sets_fields` | `openai_compat.rs` | Tool delta |
| `accumulate_tool_deltas_does_not_overwrite_with_empty_id_or_name` | `openai_compat.rs` | Tool delta |
| `set_models_is_returned_by_list_models` | `mock.rs` | Mock provider |
| `set_model_rejects_empty_string` | `mock.rs` | Mock provider |
| `metered_provider_forwards_model_getter_and_setter` | `latency.rs` | Latency decorator |
| `complete_stream_records_ttft_and_final_event` | `latency.rs` | Latency decorator |
| `has_json_schema_is_false_for_text_format` | `provider.rs` | Request type |
| `has_json_schema_is_true_for_constraining_schema` | `provider.rs` | Request type |
| `has_json_schema_is_false_for_shapeless_schema` | `provider.rs` | Request type |
| `complete_stream_emits_no_tokens_for_empty_content` | `provider.rs` | Streaming |
| `complete_stream_emits_no_tokens_for_pure_tool_calls` | `provider.rs` | Streaming |
| `window_line_offset_on_line_2_is_correct` | `provider.rs` | Error window |
| `window_truncates_long_before_and_after` | `provider.rs` | Error window |
| `window_omits_ellipsis_at_exactly_radius` | `provider.rs` | Error window |
| `window_omits_tail_ellipsis_at_exactly_radius` | `provider.rs` | Error window |
