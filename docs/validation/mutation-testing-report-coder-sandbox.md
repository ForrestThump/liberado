# coder-sandbox — Mutation Testing Report

**Date:** 2026-07-30

| Metric | Value |
|--------|:-----:|
| Source files | 1 (`src/lib.rs`) |
| Tests before | 8 |
| Tests after | 13 |
| Viable mutants | 35 |
| Caught | 34 |
| **Catch rate** | **97.1%** |
| False positives | 1 |

## Survivors (1)

### `capped_utf8` — `>` vs `>=` (line 334)

`Vec::truncate(n)` is a no-op when `n >= self.len()`. The `>=` mutant therefore behaves
identically to `>` at every possible input — truncating `len` bytes when `len == max`
leaves the vec unchanged. **False positive.** Same operator class as the `coherence.rs`
false positives from Phase 3 of the master plan.

## Patches Applied

| Mutant | Test Added | What it guards |
|--------|-----------|----------------|
| `DockerWorkspace::resolve_path` → `Ok(Default::default())` | `docker_workspace_resolve_path_delegates_to_host` | Docker delegate returns a path under root |
| `Component::CurDir` match arm deleted | `resolve_path_accepts_curdir_prefix`, `resolve_path_accepts_intermediate_curdir` | `.` prefix and `./` intermediate components accepted |
| `docker_volume_arg` → `String::new()` / `"xyzzy".into()` | Workspace volume assertions in `docker_workspace_builds_docker_run_args` | Volume mount format `host:/workspace`, not read-only, starts with root |
| `docker_path` → `String::new()` / `"xyzzy".into()` | Same assertions | `docker_path` used inside `docker_volume_arg` |
| `capped_utf8` `>` → `>=` | `capped_utf8_passes_through_at_exact_boundary`, `capped_utf8_passes_through_below_boundary` | Exact boundary and below-boundary behavior (false positive, but tests add clarity) |

## Notes

- The 4 "unviable" mutants were already not generating (zero-test-coverage regions).
- Single-file crate: all tests live in the same module as the production code.
- `normalize_docker_path` has no mutants at all — it's a one-liner `path.replace('\\', "/")`.
