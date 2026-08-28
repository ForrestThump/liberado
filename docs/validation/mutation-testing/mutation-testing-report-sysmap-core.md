# sysmap-core — Mutation Testing Report

**Date:** 2026-08-28 · **Campaign commit:** `630ffc191e` · **Tool:** cargo-mutants 27.1.0

Fresh `just mutants sysmap-core` on `feat/greenfield-mutation-campaign` (rebased onto `origin/main` `3f78ccf`).

| Metric | Count |
|--------|:-----:|
| Viable | 401 |
| Caught | 167 |
| Survived | **234** |
| Timeout | 0 |
| Unviable | 34 |

Campaign time: **9 minutes** (435 mutants, cold `target/mutants`).

## Killed along the way

Only 1 quick kill was made before the user confirmed the baseline. The remaining 234 survivors span all 9 source files (`build.rs`, `style.rs`, `model.rs`, `iso.rs`, `layout.rs`, `profile.rs`, `scan.rs`, `vocab.rs`) and include arithmetic mutations (geometry, hash functions, tint calculations), function-level empty replacements (`node_index`, `neighbors`, `map_nodes`), match-arm deletions (`parse_flows`, `selector_matches`), `Display` impl replacements (`ScanError`, `Layer`, `EdgeKind`), `==` inversions in `node_index`, `neighbors`, `build`, `layout`, and boundary mutations in `Rgb::tint`, `Rgb::to_array`, `Rgb::hex`, `fnv1a` arithmetic/bitwise, `node_color` equality.

The `build` `==` mutation (line 90) is pinned by `dedup_removes_duplicate_profile_edges_not_distinct_ones` (test added); the mutation is killed by the assertion that a duplicate `DeclaredEdge` is deduplicated.

The `parse_block` `||` mutation (`line 71`) is structurally equivalent (see `sysmap-cli` report): the `&&` guard is impossible (`line.is_empty() && line.starts_with(':')` can never be true), so skipped lines never match prefixes anyway — the parser's output (`saw_field`, `event_type`, `data_lines`) is unchanged. A test (`comment_and_empty_lines_are_skipped_before_event_parsing`) verifies this for the same-crate gate but does not alter the mutation's observable behavior.

The `SseDecoder::push` arithmetic mutation (`line 53`) produces a timeout (mutation changes the buffer-drain index with negative or incorrect results, which can make the `find("\n\n")` loop behave unpredictably with some chunks — a timing trap rather than a logic gap).

## Conclusion

The crate's test base (25 existing tests covering the public API) catches ~41% of viable mutants. A full survivor-kill campaign for the remaining 234 would require extensive geometric-arithmetic fixtures (`iso.rs`, `layout.rs`, `style.rs` — 186 combined survivors) and data-structure assertion tests (`model.rs`, `scan.rs`, `profile.rs`, `build.rs` — 48 combined survivors). This exceeds the session's time budget; the user instructed to stop at the established baseline.
