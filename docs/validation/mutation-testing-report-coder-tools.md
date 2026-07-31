# coder-tools — Mutation Testing Report

**Date:** 2026-07-30

| Metric | Value |
|--------|:-----:|
| Source files | 1 (`src/lib.rs`) |
| Tests before | 10 |
| Tests after | 21 |
| Viable mutants | 59 |
| Caught | 55 |
| **Catch rate** | **93.2%** |
| False positives | 4 |

## Survivors (4 — all false positives)

| Line | Mutant | Reason |
|:----:|--------|--------|
| 82 | `invoke_json_for_backend` → `Ok(Default::default())` | Thin delegator wrapping `invoke_json`; zero logic beyond delegation |
| 507 | `default_diff_mode` → `String::new()` / `"xyzzy".into()` | Constant function returning `"patch"`; any string works as serde default |
| 539 | `cap_bytes` `>` → `>=` | `Vec::truncate(n)` is no-op when `n >= self.len()` — same operator class as coder-sandbox's `capped_utf8` false positive |

## Patches Applied (11 new tests)

| Survivors Fixed | Test Added |
|----------------|------------|
| `list_files` → `Ok(Default::default())`; `!` deleted | `list_files_returns_workspace_contents`, `list_files_respects_limit` |
| `search_text` `>=` → `<` | `search_text_respects_limit_and_multi_match_file` |
| `edit_file` `>` → `>=` | `edit_file_writes_unique_old_text` |
| `apply_patch` `>` → `<` | `apply_patch_rejects_ambiguous_edit` |
| `git_status` → `Ok(Default::default())` | `git_status_returns_result` |
| `git_diff` → `Ok(Default::default())` | `git_diff_returns_result` |
| `catalog` → `vec![]` | `catalog_contains_expected_tools` |
| `invoke` → `Ok(String::new())` / `Ok("xyzzy".into())` | `invoke_round_trips_through_invoke_json` |
| `path_allowed_to_write` → `true` | `write_blocked_by_path_policy` |
| `walk_files` `+=` → `*=` + `>=` → `<` | `walk_files_respects_limit` |
| `default_limit` → `1` | Caught by `list_files_respects_limit` (test both default and explicit limit) |

## Notes

- The `walk_files_respects_limit` test caught both `+=` → `*=` and `>=` → `<` simultaneously
  (the two mutants at lines 567-568), since either one causes `walk_files` to visit 0 files.
- `cap_bytes` (line 539) has the same `>` / `>=` / `Vec::truncate` false-positive class already
  documented in the coder-sandbox report.
- The 4 unviable mutants were in zero-test-coverage regions (not exercised by any test).
