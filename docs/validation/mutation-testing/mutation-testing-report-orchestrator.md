# Mutation Testing Report — `liberado-orchestrator`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 70 | 77 | **+7** |
| Mutants tested | 103 | 103 | — |
| Caught | 73 | 78 | **+5** |
| Missed | 11 | 6 | **−5** |
| Unviable | 19 | 19 | — |
| Catch rate (of viable) | 86.9% | 92.9% | **+6.0pp** |

No real code bugs were found during triage.

## Tests Added

| Test | Area |
|------|------|
| `vault_delivery_path_accepts_valid` | Delivery path validation |
| `vault_delivery_path_rejects_bare_filename` | Delivery path validation |
| `vault_delivery_path_rejects_empty_segments` | Delivery path validation |
| `looks_like_a_document_accepts_at_threshold` | Document detection — exact boundary |
| `looks_like_a_document_rejects_below_threshold` | Document detection — below floor |
| `looks_like_a_document_accepts_bullets` | Document detection — bullet structure |
| `looks_like_a_document_accepts_numbered_list` | Document detection — numbered list |

## Remaining Missed Mutants (6)

| Location | Mutants | Reason |
|----------|---------|--------|
| `lib.rs:539` | `<` → `<=` in `delivery_consequence_ok` | Consequence enum boundary; needs a Consequence exactly at the gate threshold |
| `lib.rs:926` | `==` → `!=` in loop profile selection | Integration-level; needs a dispatch scenario that checks exact vs semantic profile |
| `lib.rs:1385` | `NoMcpRuntime::catalog → vec![]` | Method already returns `vec![]` — functionally identical |
| `lib.rs:1516` | Guard → `true`, `&&` → `||` in `vault_delivery_path` | Guard replacement; existing tests already exercise the correct path |
| `lib.rs:1552` | `delete !` in line-count check | Only diverges when empty lines are present in a long enough body |
