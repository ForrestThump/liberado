# coder-agent — Mutation Testing Report

**Date:** 2026-08-24
**Status:** historical
**Authority:** evidence
**Scope:** `liberado-coder-agent`, lib tests only (`just mutants-agent`; e2e hangs under
cargo-mutants' temp environment, so kills must land in `#[cfg(test)]` sibling files).

## Campaign history

| Ledger row | Survived | Caught | Timeout | Note |
|---|---:|---:|---:|---|
| markdown-era seed | 150 | 176 | 2 | no commit SHA; stale by an unknown amount |
| `0e14ecc` (fresh baseline) | 216 | 519 | 4 | 739 viable |
| `770db3e` | 17 | 679 | 4 | 700 viable |
| `0faa816` | 12 | 684 | 4 | drift survivors killed |
| `e6abb77` (final) | 12 | 684 | 4 | one accessor kill; see ledger |

Viable counts differ between rows because killing the `ProductionSurface`
field-deletion class **removed mutants**, not just caught them: the entry
constructors now enumerate every field explicitly, so a dropped binding is a
compile error and cargo-mutants marks the site unviable.

## What was killed, by module

- **repair_feedback** — hint table, findings-less formatting branch, clip-window
  head anchoring, package-vs-generic failure markers, classification arms and
  guards, passthrough envelope, focus-block attempt windows, sha256 signatures.
- **progress** — fatal naming/messages, take-once latch, what clears a stall
  (successful edits only — not reports, not failed writes), counter scoping for
  unknown tools, once-per-stall nudges, nudge-to-fatal grace band, validation
  signature/pass parsing.
- **roles / trace** — repair-role selection boundaries, whitespace-prompt-file
  fallback, goal assembly sections, char-count truncation; session-id
  sanitisation and TurnTracer event recording including the live Token mirror.
- **completion_gate** — reviewer naming, vote flattening with coercion audit,
  contract rendering, trace/fanout observers, refutation-history bounding, the
  strategist chain end to end.
- **assemble** — surface hashline override plus explicit field enumeration
  (production change).
- **session_pack/build** — git-repo detection (`.git` itself is not a work
  tree), fan-out nesting guard, mode turn bounds, restricted-mode notices,
  guidance stop words, contract integrity refusal; full fan-out pipeline via a
  committing backend double (branch tips, clean merges, ship preflight failing
  a green fan-out).
- **session_pack/policies / intake** — payload policy overrides, resume
  announcement, coherence-redraft budget vs human clarify rounds, round
  boundaries, intake context assembly, freeze-rendering sections.
- **preflight_hook / remediation** — ship-required decision table, baseline
  cache location, merge-base discovery, green-run event shape, failure-detail
  scoping, pre-existing-failure non-blocking, remediation records.
- **coding_goal** — the `force_host_local` accessor feeding workspace selection.

## Final survivor list (verbatim from the recorded run)

`lib.rs` 245, 265(×2), 345, 414, 1032 · `cold_review.rs` 233, 251 ·
`fanout.rs` 699 · `session_critic.rs` 222 · `verify_pipeline.rs` 303, 349 ·
`preflight_hook.rs` 218 — each justified in the table above.

## Harness notes for the next agent, overall-verdict conjunction, child turn
  budget, LLM conflict resolution staging/committing, fence stripping.
- **lib.rs** — backend naming, revision-retry and retryable-error boundaries
  driven through scripted model turns, strategist consultation and directive
  propagation into the next attempt's prompt, trace reading/ending detection,
  diff assembly truncation markers and newline hygiene, checkpoint emission,
  gate-skipping for unchanged trees, critic wiring, pre-existing-failure
  softening.

## Accepted survivors (equivalent or harness-blocked)

| Location | Mutant | Why it stands |
|---|---|---|
| `lib.rs:245` | delete `!` in summary dedup | Dead guard: `run_attempt` already stamps "critic requested revision" into the summary upstream. |
| `lib.rs:265` | `+`→`*`, `<`→`<=` in retry guard | Inside `for offset in 0..max_attempts`, `offset * 1 < max_attempts` holds exactly when `offset + 1 < max_attempts`; the `<=` boundary lands where the loop has already exited. Both verified against the live mutants in the final campaign. |
| `lib.rs:345` | delete `!` around `review.is_clean()` | Gates a `tracing::info!` only. |
| `lib.rs:414` | delete `NoChanges` match arm | Arm returns `false`; the catch-all below returns `false`. |
| `lib.rs:1032` | delete `!` before baseline comparison | Softening a *passing* pipeline is an identity; killing the inverted form needs a verifier run with a real pre-existing failure and a computed baseline — harness-sized follow-up. |
| `cold_review.rs:233/251` | blank-blob leak-guard bangs | `reject_author_context` applies the identical predicate before any prompt assembly; the in-message guard can never fire differently. |
| `session_critic.rs:222` | `>` → `>=` in review span | `end == start` requires one character to be both braces. |
| `verify_pipeline.rs:303` | delete `Err(e)` arm | Only reachable if the git binary is missing; a no-commit repo is swallowed to empty upstream. |
| `verify_pipeline.rs:349` | `>` → `>=` in char-boundary walk | Position 0 is always a boundary, so the equal case exits anyway. |
| `preflight_hook.rs:218` | `-` → `+` in pre-existing count | The count prints only when `new` is empty, where sum equals difference. |

## Harness notes for the next agent

- **Fan-out e2e needs commits, not stubs.** A scripted backend that *commits a
  file on its branch* gives children real tips so merges come back clean; that
  is what makes the report-stage survivors observable. Worktree paths are
  relative under `LIBERADO_DATA_DIR` — scope it per test with the existing
  `DATA_DIR_ENV_LOCK` + restore guard.
- **Attempt-loop mutants need model-turn scripts, not unit seams.** A revision
  on a non-final attempt costs five provider responses (write, report, critic
  refute ×1 legacy or gatekeeper, second-attempt report, critic accept); assert
  both the outcome and that the directive text reaches a later request.
- **Line numbers drift between campaigns.** Verify each survivor's source line
  before writing a test against a name taken from an older outcomes file — two
  batches here initially targeted the wrong operator.

The raw survivor lists for each campaign live next to this report's git
history; `mutants.out/outcomes.json` from the final run is the authority for
what remains.
