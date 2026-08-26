# coder-tools — Mutation Testing Report

**Date:** 2026-08-25 · **Campaign commit:** `4ffd4b4e` · **Tool:** cargo-mutants 27.1.0

The historical "4 survivors" figure below came from a single-file scope under a 3-second
test floor: 141 of 526 viable mutants timed out rather than ran. The honest re-baseline
widened scope to the whole crate and raised the floor to 30 seconds (`TIMEOUT_OVERRIDES`).
That exposed **966 viable mutants**, 276 of which survived at `37440e4`.

| Metric | 2026-07-30 | Re-baseline | Now |
|--------|:---------:|:-----------:|:---:|
| Viable | 59 | 966 | 966 |
| Caught | 55 | 673 | **863** |
| Survived | 4 | 276 | **81** |
| Timeout | 0 | 17 | 22 |
| Unviable | 26 | 89 | 89 |

## Fixed along the way

### A real production bug: the TypeScript repo-map query never compiled

`query_source("typescript")` mixed two node types this grammar does not have:
enum names are `(identifier)` (not `(type_identifier)`) and function expressions are
`(function_expression)` (not `(function)`). `Query::new` failed on every TS/TSX file,
`extract_tags` returned empty, and repository maps silently lost every TypeScript and
TSX file. Two MatchArm-deletion mutants on that match arm were unkillable *because the
arm already did nothing*. Fixed and pinned by per-language extraction tests.

### Survivor kills by module

- **repo_map.rs** (112 → 7): golden PageRank distributions — including a config whose
  personalization sums to three, so normalization is observable rather than a fixed
  point — personalization boost matrix (path-only / symbol-only / chat), graph edge
  weights and pair deduplication, tag name-length bounds with one-based lines,
  ts/js/jsx/go query health, task-term classification, rank bar formatting,
  min-source-files boundary, path-vs-body scoring under the file cap, context-map
  routing and evidence ordering (`name×4 > file×2 > snippet×1`, zero-score exclusion,
  `min(max/3, 384)` budget), truncation message arithmetic counted from the overflow
  point, both strict-greater budget guards pinned at exact fit (header and blank lines
  ride free — token accounting starts at zero), and walk depth/skip-list/size ceilings.
- **hashline.rs** (81 → 16, of which 13 equivalents): exact normalization output,
  full-tag golden hash with clamped lengths, JSON-quoted error previews,
  MV/section-header termination of PUT bodies, blank-row layout rules (per-blank-row
  empty payloads; leading/trailing blanks are layout), comment skipping between ops,
  exact bounds-validation messages distinguishing an empty file from an out-of-range
  anchor, BOF inserting into one-line non-empty files instead of replacing them,
  EOF phantom handling with first-changed tracking.
- **git.rs** (15 → 2): Display renders the message; agent commits carry the liberado
  identity through `--format`; named-file commits stage only those files; empty
  pathspecs rejected at the tool layer; local push/fetch keep stdout quiet;
  fast-forward merges narrate; log format routing passes `%s` through while `""`
  falls back to the walk.
- **fuzzy_match.rs** (14 → 4): golden Levenshtein pairs, taller-than-content targets,
  equal-height near misses, tied windows stay ambiguous with earliest-best, odd-unit
  indent rounding.

## Accepted residue (equivalent or transport-bound)

Documented per-function in test-file comments where the reasoning fits:

- **pagerank/personalization scalar cancellation**: base shares cancel under
  normalization; the total-positive invariant always holds for positive inputs.
- **hashline**: nested re-checks subsume weakened guards (`line > line_count` inside a
  coarser arm); unsigned arithmetic cannot go negative; peek loops converge from
  either start; `i != 0 ≡ i > 0`.
- **walk caps**: `MAX_SCAN_FILES` boundary needs a 5001-file fixture — observationally
  equivalent below the cap, not worth slowing every mutant run for.
- **fuzzy ties**: `>` vs `>=` into `second_best` stores the same f64 either way.
- The remaining bulk sits in `lib.rs`'s tool surface (grep/read/untracked-section/
  preflight plumbing) — next campaign tier.
