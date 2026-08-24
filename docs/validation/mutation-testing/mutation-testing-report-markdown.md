# markdown — Mutation Testing Report

**Date:** 2026-08-23
**Status:** current · **Authority:** ledger rows at `0e14ecc` (baseline) and `c23c9ca7` (final), branch `fix/markdown-mutant-survivors`

## Summary

| Metric | Baseline (`0e14ecc`) | Final (`93645e4`) |
|--------|:-----:|:-----:|
| Viable mutants | 126 | 118 |
| Caught | 48 | **114** |
| Missed | 33 | **2** |
| Timeouts | 45 | **2** |
| Unviable | 20 | 21 |
| Tests | 11 | 33 |

The headline finding was not weak tests — it was a **termination bug in the production
parser**. Every one of the 45 baseline timeouts was `parse_inline` hanging forever on an
input it could not parse, not a slow test or a cold cache.

## Timeout forensics

The user-visible question was whether the *unmutated baseline* was timing out (the
`liberado-cli`/`liberado-memory-mcp` failure mode, fixed by timeout-table entries). It was
not, and no table entry was added:

- `mutants.out/outcomes.json` baseline scenario: `Build=Success 0.60s`, `Test=Success 0.30s`.
- `mutants.out/debug.log`: zero `phase=Baseline` timeouts; all 45 timeouts were
  `phase=Test` with `process_status=Timeout elapsed≈3.01s` — every mutant test run pinned
  at exactly the `--timeout 3.0` cap from `mutants_cmd.rs`.

### Root cause (code fix, not a test fix)

`parse_inline`'s main loop had two ways to advance: consume a parsed span (`i = next`) or
scan plain text up to the next markup byte. When the current byte **was** a markup starter
(`*`, `[`, `` ` ``) but opened nothing valid — unterminated emphasis, `[text]` without
`(url)`, an unclosed code span — neither branch moved the cursor and the loop spun
forever. Any mutant that broke a successful parse on existing test inputs therefore turned
a passing test into a hang instead of a failure. Even block-level mutants timed out this
way: `markdown_to_lines` feeding a stray fence line into `parse_inline("`…`")` hit the same
stall.

Fixes applied to `crates/markdown/src/lib.rs`:

1. **Guaranteed progress.** Each iteration now consumes at least one byte: when no span
   opens, the parser emits the run up to the next markup starter, or the single starter
   byte itself as a literal `NONE` span when that starter matched nothing. Malformed input
   renders literally instead of stalling the caller.
2. **Non-advancing cursors rejected.** Span dispatch requires `next > i`; a mutated parser
   returning a degenerate cursor falls through to literal consumption instead of looping.
3. **Empty spans rejected.** `closed_from` wraps `find_inline_end`, refusing an adjacent closer;
   `parse_bold`/`parse_italic`/`parse_code`/`parse_link` therefore never emit empty inner text; `"a **b"` used to emit a zero-width italic span where the second
   star closed against nothing.
4. `find_inline_end` rewritten over `windows()` (escape handling preserved), removing the
   manual index arithmetic whose mutation could only ever hang.

Per the standing rule, each surviving-mutant kill below was verified by applying the exact
mutation by hand, running the suite, watching it fail, and restoring — including
classification of hang-class probes via process signal, after cargo's exit-code reporting
initially mislabeled two SIGKILLed runs as ordinary failures.

## Fixed survivors (31 missed + all killable timeouts)

| Cluster | Tests added | What they pin |
|---|---|---|
| Unterminated markers | `unterminated_star_renders_literally`, `lone_markup_characters_render_literally`, `unterminated_code_span_is_literal`, `bracket_without_paren_is_literal`, `unclosed_bracket_is_literal`, `trailing_bracket_without_paren_is_literal`, `double_star_without_closer_falls_back_to_literals`, `literal_star_then_valid_link_resumes_parsing` | Literal fallback for malformed markup; scanning resumes correctly after a fallback |
| Boundary arithmetic in span parsers | `bold_at_line_end_exact`, `bold_then_tail_exact`, `link_at_line_end_exact`, `minimal_link_exact`, `code_span_boundaries_exact`, `italic_at_line_start_exact`, `empty_markup_spans_render_literally` | Exact span sequences at start/end-of-line, killing `+`→`*`/`-` and `<`→`<=` mutants on cursor math |
| Precedence and escape rules | `italic_never_opens_at_second_star_of_a_pair`, `escaped_stars_render_as_text`, `lone_star_before_later_bold_stays_literal` | The `bytes[i - 1] != b'*'` rejection and `\`-escape skip in `find_inline_end` |
| Scanner depth logic | `nested_brackets_in_link_text`, `nested_parens_in_link_url` | Match-arm deletions, `depth == 0` guard swaps, and `depth ±= 1` operator swaps |
| Block-level gaps | `horizontal_rule_forms` (`***`/`___`), `empty_input_yields_no_lines` | The `||`→`&&` hr-chain mutant; empty-input contract |

## Accepted residues (4)

All four were individually reproduced and classified by hand before acceptance.

| Location | Mutant | Class | Why accepted |
|---|---|---|---|
| `lib.rs:275` `parse_inline` bold dispatch | `>` → `>=` | Equivalent | The guard rejects degenerate cursors that only arise under *compound* mutation; for any single mutation of the parsers, a returned `next` is always `> i`. Swaps that change behavior on valid input (`==`, `<`) are caught by positive tests (verified). Killing `>=` would require deleting the guard, which reconverts parser mutations from failures back into hangs. |
| `lib.rs:282` `parse_inline` link/code dispatch | `>` → `>=` | Equivalent | Same reasoning as line 275. |
| `lib.rs:297` `parse_inline` fallback step | `start + 1` → `start - 1` / `start * 1` | Liveness tautology (times out) | Verified HANGS by hand. This is the single-byte-progress operation that guarantees termination; mutating it to identity (`*1`, `/1`) stalls exactly the new fallback tests, and `-1` underflows or cycles. Any black-box test distinguishing these variants would itself have to not terminate. Restructuring cannot remove it: every formulation of "advance one byte" bottoms out at one mutable `+1`. |

The two timeout rows are the mutation suite's view of the fix itself: the campaign proves
the progress guarantee is the *only* remaining unguarded liveness dependency.

## Campaign provenance

```text
baseline: cargo mutants -p liberado-markdown --cap-lints true --timeout 3.0 --minimum-test-timeout 30 --in-place
          at 0e14ecc1c7521034c9142782a0306861584acb29 → viable 126, caught 48, missed 33, timeout 45
final:    same command at c23c9ca7bd71313c416550882012084fa0ff5882
          → viable 118, caught 114, missed 2, timeout 2

An intermediate row at `93645e4` (viable 139, caught 135) preceded a complexity-budget
refactor: the non-empty-span rule moved into a named `closed_from` helper and raw index
bounds became `get()` comparisons, keeping every parser at or below its CRAP baseline while
shrinking the mutable-arithmetic surface (139→118 viable). Residues unchanged.
```

Ledger rows appended (never edited); raw `mutants.out/` discarded after triage per the
three-layer rule. Commits: baseline ledger row first, then
`fix(markdown): guarantee parse_inline termination…`, then two test commits.

---

## Stretch goal — fresh `executor` campaign (2026-08-23)

**Status:** recorded · **Authority:** ledger row at `f6597d1`

| Metric | Value |
|--------|:-----:|
| Viable mutants | 332 |
| Caught | 314 |
| Missed | 18 |
| Timeouts | **0** |
| Unviable | 57 |

The crate grew ~2× since the markdown-era seed (168 viable, 29 survived, commit unknown);
a fresh run resets its drift clock and shows the survivor pool already shrank to 18 with no
fixes in this campaign. No timeouts — the executor suite needs no timeout-table entry.
Survivor triage left for a dedicated pass; raw `mutants.out/` discarded after ingest per the
three-layer rule.

Campaign litter note: the run created `crates/executor/proptest-regressions/` (mutant-induced
proptest failures); deleted untracked, matching every other crate that does not track them.
