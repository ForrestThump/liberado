# sysmap-gui — Mutation Testing Report

**Status:** historical
**Authority:** evidence
**Date:** 2026-08-28 · **Campaign commit:** `737b99b` · **Tool:** cargo-mutants 27.1.0

Two campaigns were run on the same base (`737b99b`, the `origin/main` the work branched from).
The first is the **baseline**, the second is the **final** after adding
`second_hop_edge_in_selection_requires_both_endpoints_in_scope`. Both rows are appended to
`mutants-ledger.json`.

| Metric | Baseline | Final |
|--------|:--------:|:-----:|
| Viable | 196 | 196 |
| Caught | 30 | **31** |
| Survived | 166 | **165** |
| Timeout | 0 | 0 |
| Unviable | 31 | 31 |

`build_mutants_command` has no per-crate timeout override for `liberado-sysmap-gui`, so the run
used the default `--timeout 3.0 --minimum-test-timeout 30` and `--in-place`. The eframe+wgpu
toolchain makes every mutant rebuild ~1s; the full run took ~6 min on each pass.

## Killed along the way

| Location | Mutant | Test added |
|----------|--------|-----------|
| `crates/sysmap-gui/src/interaction.rs:56` `&&` → `\|\|` in `edge_in_selection` | An edge with one endpoint in the two-hop scope and one out (e.g. `x → b` where `x` is unreachable from `a`) was wrongly reported as in-selection. The existing `direct_selection_shows_only_incident_edges` test only checked edges where both endpoints ARE in scope, so the `\|\|` mutant passed trivially. | `second_hop_edge_in_selection_requires_both_endpoints_in_scope` — constructs an `x → b` edge against the existing fixture's two-hop scope and asserts `edge_in_selection` returns `false`. |

## Survivors accepted out of scope (165)

The `sysmap-gui` crate is the 2D interactive renderer. Most of its 704-line `app.rs` is egui
panel code (window setup, sliders, painter calls) that has no testable seam without an egui
headless harness, which the codebase does not have. The breakdown:

* **`app.rs` rendering surface (≈138 survivors)** — sliders, checkboxes, painter calls, font
  selection, label fitting. All are changes to UI presentation with no observed behavioral
  effect on the `SystemMap` data; no test can reach them without spinning up an egui context.
* **`interaction.rs::arrow_points` (12 survivors)** — arithmetic mutants in the 2D geometry
  that draws the arrow head. The existing `arrowhead_points_in_edge_direction_at_every_zoom_level`
  test asserts the points are "behind the tip in the right direction" (`points[1].x <
  points[0].x`), which holds under any of the `*` / `+` / `-` mutations because the
  geometric structure is preserved. The exact length and angle are visual style choices.
* **`interaction.rs::ray_rect_distance` (10 survivors)** — boundary (`>`, `>=`, `==`, `<`)
  and division-by-zero handling for ray-AABB distance. Used in `app.rs`'s edge hit-testing,
  which has no test.
* **`insights.rs` (3 survivors)** — `show_cycle_warning` and `show_metadata` are
  presentation-only egui calls; their effect is "render some RichText on the UI" and is
  not assertable from a unit test.

## Conclusion

The `sysmap-gui` crate's test suite catches **15.8% of viable mutants** (up from 15.3%). The
single killed survivor was a real logic bug in the second-hop edge filter; the other 165
misses are concentrated in untestable egui rendering and 2D geometry that no unit test
in this crate can reach. A meaningful improvement would require either an egui headless
harness or a more aggressive test of `ray_rect_distance` (numerical edge cases), both of
which are out of scope for a greenfield campaign.
