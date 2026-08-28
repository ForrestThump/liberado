# sysmap — Mutation Testing Report

**Status:** historical
**Authority:** evidence
**Date:** 2026-08-28 · **Campaign commit:** `1a452e09ff15091ed19fea8305bd7f7770e078bf` · **Tool:** cargo-mutants 27.1.0

The original rows recorded the baseline and first test pass under a commit that did not contain
the dirty-tree changes. They remain append-only history. The reviewed rerun at
`1a452e09ff15091ed19fea8305bd7f7770e078bf` contains the `find_repo_root` refactor, its tests,
the complete `runtime_layer` table test, and a direct `repository_root` wrapper test.

| Metric | Original baseline | Reviewed rerun |
|--------|:-----------------:|:--------------:|
| Viable | 30 | 32 |
| Caught | 12 | **22** |
| Survived | 18 | **10** |
| Timeout | 0 | 0 |
| Unviable | 11 | 11 |

`build_mutants_command` has no per-crate timeout override for `liberado-sysmap`, so the reviewed
run used the default `--timeout 3.0 --minimum-test-timeout 30` and `--in-place`. It tested 43
mutants in 82s after a 1s baseline build and 2s baseline test.

## Killed along the way

| Location | Mutant | Test added |
|----------|--------|-----------|
| `crates/sysmap/src/lib.rs:90` `&&` → `\|\|` in `find_repo_root` | first directory with `crates/` returned as the root | `find_repo_root_walks_up_through_crates_only_directories` — nested directory has `crates/` but no `Cargo.toml`; the function must keep walking |
| `crates/sysmap/src/lib.rs:93` `delete !` in `find_repo_root` | the walk returns `None` after its first successful `pop()` | `find_repo_root_walks_up_through_crates_only_directories` — the walk must continue through the nested directory to the fixture root |
| `crates/sysmap/src/lib.rs:78` `repository_root` → `Ok(Default::default())` | empty `PathBuf` returned as the root | `repository_root_returns_an_absolute_workspace_root` calls the wrapper and rejects a relative empty path |
| `crates/sysmap/src/scan.rs:49-53` × 5 (one per match arm) | deleting `provider \| notifier`, `mcp \| hook`, `pool \| profile \| schedule`, `project`, or `vault` falls through to `_ => "unknown"` | `runtime_layer_maps_every_known_kind_to_its_group` — table-driven test asserts every known kind maps to its expected group (`foundation` / `service` / `kernel` / `pack` / `store`) |

### Refactor

`repository_root()` walked from `std::env::current_dir()` directly. That made the loop body
untestable without `set_current_dir` (process-global state, AGENTS.md warns against this in
tests). Refactored:

* `pub fn repository_root() -> Result<PathBuf, String>` — unchanged signature, now delegates.
* `pub fn find_repo_root(start: &Path) -> Option<PathBuf>` — pure walker, takes the start path.

`liberado-sysmap-cli` calls `repository_root()` and `resolve_config_dir(...)`; both signatures
are unchanged, so the binary is unaffected.

## Survivors accepted out of scope (10)

| Location | Mutant | Why retained |
|----------|--------|----------------|
| `crates/sysmap/src/lib.rs:43` | `BuildError::fmt` → `Ok(())` | The `Display` impl is never asserted on. The error wraps either a `ScanError` or a `sysmap_core::ScanError`; both have their own `Display`. No test reads the formatted `BuildError` string. |
| `crates/sysmap/src/lib.rs:107` | `delete !` in `resolve_config_dir` | Empty `LIBERADO_CONFIG_DIR` is treated as a valid (empty-path) config dir. Killing it needs an env-var test; AGENTS.md flags env-var manipulation in tests as a flake source. Same-crate test would race with any future test reading the var. |
| `crates/sysmap/src/lib.rs:117` | `dirs_config_dir` → `None` | Platform-config fallback not exercised by any test. The function reads `XDG_CONFIG_HOME` / `HOME` (Linux) or `APPDATA` / `USERPROFILE` (Windows). |
| `crates/sysmap/src/lib.rs:117` | `dirs_config_dir` → `Some(Default::default())` | Same. Empty `PathBuf` joined with `"liberado"` gives `"liberado"`; no test reads the platform-config-dir path. |
| `crates/sysmap/src/scan.rs:33` | `ScanError::fmt` → `Ok(())` | Same as `BuildError::fmt` — `Display` impl is never asserted on. |
| `crates/sysmap/src/scan.rs:80` | `enabled(v: &bool) -> *v` → `true` | The function is an identity (`*v`). The mutation makes every node `enabled = true`. No test reads `MapNode::enabled`. The flag flows through to consumers in `sysmap-core` / `liberado-sysmap-gui`, but no same-crate test observes the difference. |
| `crates/sysmap/src/scan.rs:117` | `topo.provider == p.name` → `!=` | Inverts which provider gets `meta["active"] = "true"`. No test asserts the active-provider meta. |
| `crates/sysmap/src/scan.rs:190` | `delete !` in `mcp_node` | `if !args.is_empty()` becomes `if args.is_empty()` — the `meta["args"]` key is only inserted when there are no args (silently inverts the intent). No test asserts the args meta. |
| `crates/sysmap/src/scan.rs:357` | `transport_label` → `""` | `meta["transport"]` ends up empty. No test reads it. |
| `crates/sysmap/src/scan.rs:357` | `transport_label` → `"xyzzy"` | Same field, observed as `"xyzzy"`. No test reads it. |

## Conclusion

The `sysmap` crate's test suite catches **68.8% of viable mutants** (up from 40%). The 10
remaining misses are concentrated in three buckets: untested `Display` impls, untested fallback
paths in config-dir resolution, and `MapNode` meta fields that no test asserts on. The
`enabled` identity function, `transport_label` constant lookup, and `mcp_node` args key are
candidates for a follow-up campaign that adds a `topology_*` integration test asserting the
expected meta values — that would knock out another 5-6 survivors cheaply.
