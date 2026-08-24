# coder-core — Mutation Testing Report

**Status:** current · **Authority:** ledger campaign at `4857a35`, branch `fix/coder-core-mutant-survivors`

| Metric | July seed | This campaign (before) | After |
|--------|:---:|:---:|:---:|
| Viable mutants | 469 | 469 | 469 |
| Caught | 341 | 341 | **458** |
| **Missed** | 127 | 127 | **10** |
| Timeouts | — | 1 | 1 |

Every campaign shown generated the same 510 mutants (469 viable + 41 unviable); only
catches moved.

## Fixed (117 mutants, every one verified KILLED before recording)

- **trace_view.rs (62→0 real):** transcript renders a marker for every `CoderEvent`
  variant; metrics terminal-state machine (last-writer-wins, guard-only fallback,
  abort summary); comparison turns number from one; resolve_trace_path exact/prefix/
  messages-export skip/not-found vs ambiguous; native run_view pairs arguments to
  results by name oldest-first and preserves call issue order; foreign importers
  (user-task/annotation split, per-turn indexing from one, tool pairing by
  tool_call_id/name with duplicate-result drop, kilo-cli part expansion incl.
  state.error bodies, OpenHands action families, success-flag inversion);
  detect_foreign_format precedence chain; divergence first-disagreement, tail
  bounds (>3 shown + exact skip count), interventions section presence/absence;
  fmt_tools/fmt_failures/fmt_ok/fmt_mutation/truncate shapes.
- **lib.rs (33):** CodingMode wire spellings + fail-closed strictest matrix +
  per-mode presets compared by value; DispatchWriteScope activity/allow/deny
  semantics with backslash normalization; infrastructure deny defaults and output
  caps; SessionReview::is_clean both directions; include_tool_names serde default;
  findings markdown ordering (outstanding → session → speculative → closed) with
  disputed labels, closed-count arithmetic, and empty-render contract.
- **intake.rs (9):** both flexible deserializers table-driven through real JSON.
- **failure_excerpt.rs (11):** cap semantics (0 = uncapped, exact fit adds no
  marker, surplus count exact), context window bounds, gutter/diagnostic
  classification, ANSI stripping, all documented failure shapes.
- **prompts.rs (3):** missing file silent / unreadable loud via all-field log
  capture with a mode-000 fixture.
- **tuning.rs (2):** empty validation_command rejected; trace_formats default.
- CLI hardening carried from earlier crates plus a coder-core timeout entry.

## Accepted survivors (10)

| Location | Mutant | Why accepted |
|---|---|---|
| `lib.rs:323` `strictest` | `>` → `>=` | Equal ranks name the same variant; picking either is identical. Equivalent. |
| `coherence.rs:243` `looks_like_a_path` | `>` → `>=` | Exhaustive search: no 3-byte word satisfies the remaining constraints (a valid extension alone needs ≥2 bytes after a dot). Equivalent. |
| `intake.rs:100` FlexString `visit_str` | body → `Ok(Default)` | serde_json routes owned strings to `visit_string`; this method is unreachable through any supported input path. |
| `intake.rs:124/128`, `229/233` `visit_none`/`visit_unit` ×4 | body → `Ok(Default)` | Original bodies return `String::new()` / `Vec::new()` — exactly `Default`. Equivalent by construction. |
| `intake.rs:182` StringOrVec `visit_i64` | body → `Ok(Default)` | `visit_seq` consumes elements as `serde_json::Value` and handles numbers itself; scalar visitors never fire for list members. Dead under serde_json. |
| `trace_view.rs:167` hash-only request arm | arm deleted | Falls through to `_ => {}` — the arm's body is already "do nothing". Equivalent. |
| `trace_view.rs:1690` `write_messages_export` | delete `!` on parent-empty guard | `create_dir_all("")` succeeds as a no-op on all CI hosts; behavior identical. |

## Process notes

- Three interrupted in-place runs left live mutations behind; each was caught by
  `git status` + clippy before any commit. The skill documents the recovery steps.
- The prompts guard test needed an all-fields log visitor: the distinguishing path
  lives in structured fields, not the message.
- One test was written environment-blind (presence check on `CARGO_TARGET_DIR`,
  which cargo-mutants itself sets) and passed under its own mutant until rewritten
  against the captured previous value.
