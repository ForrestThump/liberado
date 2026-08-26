# liberado-commands — Mutation Testing Report

**Date:** 2026-08-26 · **Campaign commit:** `72e4ab44` · **Tool:** cargo-mutants 27.1.0

The historical ledger row (`82b28558`, 21 of 39) predated the `/plan`//`/explore` coding tiers
and the focus surface (`/sessions`, `/join`, `/spawn`, `/goal`). The fresh campaign at
`5cc2574` found **105 viable mutants, 43 surviving**.

| Metric | Old row | Fresh baseline | Now |
|--------|:-------:|:--------------:|:---:|
| Viable | 39 | 105 | 105 |
| Caught | 18 | 62 | **102** |
| Survived | 21 | 43 | **3** |
| Unviable | 1 | 23 | 23 |

## What the survivors covered

- **catalog.rs**: the Telegram menu projection is now pinned to its exact nine entries in
  order; prefix helpers are pinned case-folded with byte-exact cuts; slash detection covers
  multiline and blank input; insert-only filter matches keep family umbrellas alive when the
  typed space outgrows their display name; Tab completion pins both branches — an extending
  family completes to its shared prefix (`/the` → `"/theme "`), an exhausted one jumps to the
  first match's full insert (`/s` → `"/status"`).
- **dispatch.rs**: `/join` and `/spawn` argument shapes through `splitn(3)`, lenient `/fork`
  argument handling, coding-tier payloads, and the reserved `/goal` lifecycle words.
- **handlers/status.rs**: running/stopped and attached/detached polarity in the rendered
  text, the true percentage, the zero-window placeholder (`--` behind a strict `w > 0`
  guard), and the display-cap ceiling.
- **handlers/focus.rs, theme.rs, profile.rs**: every `CommandResult` routing variant, usage
  text for all three empty-argument families, project trimming/filtering, active-theme
  labelling, reload success and error paths.
- **commands.rs**: the `plan`/`explore` wire strings — the client/pack boundary contract.

A shared [`SurvivorCtx`](../../../crates/liberado-commands/src/test_mock.rs) double lives in
`test_mock.rs`, wired from `lib.rs`, so sibling suites observe messages, cleared input,
theme bookkeeping, and status snapshots without duplicating a mock.

## Accepted residue (3 — all equivalents)

- `complete_commands` single-match shortcut (`==` on `matches.len() == 1`): the fallback
  common-prefix path computes exactly the same string for any single match.
- `complete_commands` strict-greater guard (`prefix.len() > query.len()` → `<`): given this
  const catalog, whenever the guard fires its value equals `matches[0].insert`, so both
  arms coincide. The other two mutations of that comparison (`==`, `>=`) *are* killed by the
  exhausted-prefix test.
- `starts_with_ignore_ascii_case` deleted `(None, Some(_)) => false` arm: control falls to
  the catch-all `_ => false`, same result.
