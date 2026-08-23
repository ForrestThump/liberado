# coder-agent — Mutation Testing Report

**Date:** 2026-08-23
**Status:** historical
**Authority:** evidence
**Ledger rows:** 2026-07-30 (markdown-era, `commit: null`), 2026-08-22/23 at `dc9bd0c`
(216 survived / 739 viable), and 2026-08-23 at `78c7a6d` (29 survived / 739 viable).
The ledger is the scoreboard; this file is the triage record for the campaign that took the
crate from 216 to 29 survivors.

| Metric | 2026-07-30 | dc9bd0c (before) | 78c7a6d (after) |
|--------|:---------:|:----------------:|:---------------:|
| Viable | 328 | 739 | 739 |
| Caught | 176 | 519* | 706* |
| **Survived** | **150** | **216** | **29** |
| Timeout | 2 | 4 | 4 |

\* Plus 4 timeouts counted in viable. Scope is lib-only both runs (`-- --lib`);
`mock_intake_e2e` still hangs under cargo-mutants.

## What was done

Test modules only — no production behavior changed except one structural edit:
`entry::{pack,acp,runner}_surface` now enumerate every `ProductionSurface` field instead of
relying on `..ProductionSurface::default()`. Ten "delete field" mutants were unkillable while
the functional-update syntax stood in for fields whose values happened to equal the default;
explicit bindings make those deletions compile errors (unviable), which is the honest state.

Per-mutant kills were spot-verified by hand where the reasoning was subtle (apply → watch the
new test fail → restore), and the full campaign re-run verified all of them at once.

## Accepted survivors (equivalent mutants)

Each was analyzed and left; a test cannot exist for these without changing what production does.

| Location | Mutation | Why it is equivalent |
|---|---|---|
| `lib.rs` run_attempts `attempt_offset + 1` | `+`→`*`, `<`→`<=` | The retry arm records `last_retryable = err` before `continue`, so falling out of the loop returns the same error the direct `Err(err)` arm would have returned. |
| `lib.rs` run_session_review `!review.is_clean()` | delete `!` | Gates only a `tracing::info!`; result state is identical either way. |
| `lib.rs` is_retryable `NoChanges` arm | delete arm | The catch-all `_ => false` returns the same value; the arm exists as documentation. |
| `lib.rs` run_verifier_pipeline `!pipeline.is_pass()` | delete `!` | `soften_pre_existing_test_failures` no-ops on a passing pipeline, so running it unconditionally changes nothing observable (only wasted work). |
| `progress.rs` same-tool/read-only nudge guards `==` `&&` | `&&`→`\|\|` | A `< limit` early return precedes the condition, and every reset of `*_nudged` also resets the counter below the limit — the flipped predicate is unreachable with a different truth value. |
| `repair_feedback.rs` clip anchor window via fence strip | `+`→`*` in `llm_resolve_file` | The fence-stripping expression ends in `.trim()`, which erases the one-character difference. |
| `session_critic.rs` parse brace guard `end > start` | `>`→`>=` | `find('{') == rfind('}')` requires one character to be two different ones; the equal case is unreachable. (The reversed-braces case IS killable and now has a test.) |
| `preflight_hook.rs` pre-existing count `-` | `-`→`+`, `-`→`/` | The count is only interpolated when `new.is_empty()`, i.e. subtracting `describe_failures(&empty) == 0`. `x ± 0 = x` and `x / 1 = x`. |
| `verify_pipeline.rs` git_nonempty_diff second `Err` arm | delete arm | Reachable only if `git log` fails to spawn while `git status` spawned fine moments earlier with the same cwd and PATH. |
| `verify_pipeline.rs` prefix_at_char_boundary `end > 0` | `>`→`>=` | On `usize`, `end >= 0` is always true, but `is_char_boundary(0)` is too, so the loop stops in the same place. |

## Killed by this campaign (highlights)

- **gates** — `parse_status_path` four-byte porcelain boundary.
- **finish_gate** — a red `cargo check` refuses `succeeded` with the change-failure message,
  not the host-failure one (drives real cargo against a broken temp crate).
- **remediation** — actionable findings actually dispatch exactly one recorded run.
- **intake_session** — the wire carries the real intake prompt; blank context/answers add no sections.
- **cold_review** — excerpt rendering, blank-blob isolation, post-fix green/red decisions.
- **repair_feedback** — per-marker clipping, kind fallbacks, classify_error routing,
  marked-message round-trip, earlier-attempt listing, signature hashing, per-class hints.
- **trace** — sanitized session ids, `on_turn` events + live Token mirroring.
- **roles** — repair/coder switching, criteria rendering, feedback routing by attempt,
  truncation marks, empty-file fallback.
- **preflight_hook** — plan-mode skips, baseline cache dir, merge-base resolution, passing-run
  purity, failing-step naming in failure detail.
- **progress** — fatal names/messages, latch draining, failed-write vs churn semantics,
  validate counting toward read-only, distinct-signature reset, nudge/fatal boundaries.
- **completion_gate** — reviewer name/kind labels, vote flattening, trace+fanout observers,
  contract summary, refutation history, strategist directive verbatim + best-effort.
- **session_critic** — reversed braces error not panic, transcript text verbatim, end-to-end
  review over a scripted provider (system prompt equality, three auditable combinations).
- **fanout** — branchless-child marking, overall fail-over, child attempt budget, LLM conflict
  resolution staging/committing end-to-end, empty-content refusal, exact fence stripping.
- **coding_goal** — dispatch flags round-trip.
- **assemble** — every surface argument survives into the surface; surface hashline overrides tuning.
- **lib.rs** — backend name, `ended_in_trace`, trace round-trip fail-closed, session critic
  enabled/disabled wiring, softened-pipeline signature drop, checkpoint live event,
  untracked-section budget formatting, judgment gating on outcome+files, and the whole
  attempt loop: retry-within-budget, final-refutation suffix once, retryable-error recovery,
  single-attempt error identity, last-attempt error identity, strategist threshold and its
  gate-only scope.
- **session_pack/build** — mode turn budgets, bare-repo detection, abort synonyms, restricted-mode
  notices, contract stamping, nested-fan-out refusal, terminal/wire agreement, red ship bar
  fails a green fan-out.
- **session_pack/intake** — intake context assembly, verifier provenance tags,
  contradiction/warning separation, clarify/revise/coherence budgets, resume notice.
- **session_pack/policies** — path/command policy parse-or-none semantics.
