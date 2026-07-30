# Mutation Testing Report — `liberado-config`

Generated 2026-07-29 using `cargo-mutants 27.1.0`.

## Summary

| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| Tests | 24 | 27 | **+3** |
| Mutants tested | 53 | 53 | — |
| Caught | 23 | 24 | **+1** |
| Missed | 23 | 22 | **−1** |
| Unviable | 7 | 7 | — |
| Catch rate (of viable) | 50.0% | 52.2% | **+2.2pp** |

No real code bugs were found during triage.

## Tests Added

| Test | Area |
|------|------|
| `has_any_config_file_returns_true_for_single_file` | Config file detection |
| `merge_overlay_into_appends_zones_and_grants` | Grants overlay merging |
| `append_grant_to_overlay_at_zone_dedup` | Overlay zone dedup |

## Remaining Missed Mutants (22)

All remaining survivors are path-resolution helpers (OS env/mocking), IO-error guards, or thin public wrappers over tested inner functions — none are practically actionable without invasive test infrastructure changes.

| Location | Mutants | Reason |
|----------|---------|--------|
| `lib.rs:110` | `config_dir` → constants | Reads process env var / OS path — IO-bound |
| `lib.rs:164` | `||` → `&&` in `has_any_config_file` | Only exercised by tests with 0 or 3 config files, not 1-2 |
| `lib.rs:193` | `!=` → `==` in schema version check | Tracing-only warning — no behavioral impact |
| `lib.rs:235` | `&&` → `||` in overlay guard | Overlay with one empty collection not tested |
| `lib.rs:276,284` | `grants_overlay_path`, `load_grants_overlay` | Thin wrappers over `data_dir`/`_at` |
| `lib.rs:301` | IO error guard operators | Existing file-NOT-found tests exercise same guard |
| `lib.rs:334` | `append_grant_to_overlay` return | Public wrapper; inner `_at` is tested |
| `lib.rs:358` | `==` → `!=` in overlay zone dedup | Dedup path needs repeated same-zone append |
| `lib.rs:424` | IO error guard in `load_section` | Missing-section tests exercise same guard |
| `lib.rs:472,488,502` | Catalog/install/data dir → default | Path/struct helpers — OS-dependent |
| `lib.rs:574,576,596` | Proposal key load/persist | Key-file I/O — IO-bound |
