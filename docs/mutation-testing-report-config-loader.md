# Mutation Testing Report — `liberado-config-loader`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 106 | 113 | **+7** |
| Mutants tested | 154 | 154 | — |
| Caught | 113 | 121 | **+8** |
| Missed | 26 | 18 | **−8** |
| Unviable | 15 | 15 | — |
| Catch rate (of viable) | 81.3% | 87.1% | **+5.8pp** |

No real code bugs were found during triage.

## What Was Found and Fixed

| Test | File | Area |
|------|------|------|
| `push_adds_source` | `chain.rs` | ChainLoader |
| `default_zone_alone_satisfies_zone_requirement` | `validation.rs` | Zone guard |
| `report_sink_empty_path_arg_is_refused` | `validation.rs` | Report sink guard |
| `report_sink_empty_content_arg_is_refused` | `validation.rs` | Report sink guard |
| `timeout_at_50_is_valid` | `builder.rs` | Telegram timeout boundary |
| `read_only_declares_authority` | `builder.rs` | SessionProfile |
| `write_class_returns_declared_value` | `builder.rs` | Policy lookup |

## Remaining Missed Mutants (18)

| Location | Mutant | Reason |
|----------|--------|--------|
| `validation.rs:71` | `delete !` / `==` → `!=` | ExecuteTool MCP ref check; needs test with `ExecuteTool` + unknown MCP |
| `builder.rs:84` | `schedule` → `Default::default()` | Builder setter; functionally identical |
| `config.rs:129` | `builder` → `Default::default()` | Builder constructor; functionally identical |
| `config.rs:160` | `delete field overrides` | No test resolves a profile with overrides and checks the field |
| `config.rs:212` | `enabled_session_profiles` → `vec![]` | No caller checks this method in tests |
| `config.rs:465,494` | `==` → `!=`, `delete !` | Ceiling/zone validation guards; edge cases not exercised by existing profile tests |
| `topology.rs:120,124` | `default_path_arg`, `default_content_arg` → constants | Serde field defaults; no test asserts the default string values |
| `topology.rs:266` | `&&` → `||`, `==` → `!=`, `delete !` | Tracing-warn guard only — no behavioral impact |
| `topology.rs:888` | `default_true` → `false` | Serde field default; no test asserts the default value |
