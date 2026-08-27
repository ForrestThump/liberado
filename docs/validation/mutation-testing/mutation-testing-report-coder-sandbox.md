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

---

## Update — 2026-08-23 (ledger campaign)

**Status:** current · **Authority:** ledger row at `d2e4d328` (branch `fix/coder-sandbox-mutant-survivor`)

| Metric | Value |
|--------|:-----:|
| Generated mutants | 294 |
| Caught | 248 |
| Timeouts | 5 |
| Unviable | 32 |
| **Missed** | **9** |

The crate grew ~8× since the July report; the seed-era "1 survivor / 35 viable" row no longer
described reality. Fresh campaign recorded, then survivors fixed in three passes.

### Fixed (31 mutants across four passes)

`command_grants` delegation (Docker/Host/Worktree), spaced-deny-rule stem guard,
UTF-16LE/BOM decoding incl. short-buffer rejection, offload-id uniqueness,
`truncate_head`/`char_boundary_at_or_before`/`head_tail_preview` boundary math,
durable session-worktree create/reuse/self-heal, `run_git_best_effort` observability
(signature now returns the inner `Result`; a deleted body cannot compile),
ShadowGit snapshot parent-chaining, restore clean-exclusions for nested side repos
(intermediate-dir sentinel), `remove_worktree` happy path + locked-worktree fallback,
`declared_path_dep_roots` segment selection, `cap_log` boundary tails, bare-`RUSTSEC-`
prefix rejection, `run_step_shell` stdout/stderr join, `CargoTargetDirGuard::drop`.

Every fix was verified by running its mutant against its test (KILLED) before recording.

### Accepted survivors (9)

| Location | Mutant | Why accepted |
|---|---|---|
| `lib.rs:127` `Workspace::command_grants` default | body → `Default::default()` | The default body already is `CommandGrantSet::default()`. Equivalent by construction. |
| `lib.rs:166` `impl Debug for CommandGrantSet` | → `Ok(Default::default())` | `write_str(..)` returns `Ok(())`; `Result::default()` is `Ok(())`. Equivalent. |
| `lib.rs:600` `truncate_head` | `>` → `>=` | On `usize`, `end >= 0` is always true; loop still exits via `is_char_boundary(0)`. Equivalent. Same class as the July `capped_utf8` false positive. |
| `lib.rs:608` `char_boundary_at_or_before` | `>` → `>=` | Identical reasoning to line 600. Equivalent. |
| `lib.rs:734:16` `ensure_session_worktree` | delete `!` in race arm | Distinguisher requires winning a check-then-act race against `git worktree add`. Unobservable without concurrency injection; the arm exists to make a lost race *not* fail. |
| `lib.rs:734:32` `ensure_session_worktree` | `&&` → `\|\|` in race arm | Same race-window reasoning as above. |
| `checkpoint.rs:126:30` snapshot parent guard | `&&` → `\|\|` | Needs `rev-parse HEAD` to succeed while printing `fatal` (or empty) on stdout — not producible through public inputs. |
| `checkpoint.rs:149:37` restore id guard | `\|\|` → `&&` | With `&&`, an empty/`-`-prefixed id proceeds to git and still fails into `CheckpointError::NotFound` with the identical payload. Externally equivalent. |
| `preflight.rs:403` `cap_log` | `<` → `<=` | Loop already exits at `i == s.len()` because `is_char_boundary(len)` holds. Equivalent. |

### Process notes (friction found during this campaign)

- cargo-mutants' temp-copy mode cannot work in this repo at all (gitignored siblings);
  `--in-place` is now unconditional in `mutants run`.
- An interrupted `--in-place` run leaves live mutations and test litter behind; two of the
  three interruptions here did. Check `git status crates/` after any kill.
- A completed-but-crashed outcomes file must not append to the ledger; `mutants record` now
  refuses zero-viable rows.
