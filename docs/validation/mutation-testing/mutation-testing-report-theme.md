# theme — Mutation Testing Report

**Date:** 2026-08-24
**Status:** historical
**Authority:** evidence
**Scope:** `liberado-theme`, full lib (single-file crate, 41 viable mutants).

## Campaign history

| Ledger row | Survived | Caught | Unviable | Note |
|---|---:|---:|---:|---|
| `82b28558` | 11 | 28 | 2 | counts were fresh; no survivor list survived |
| `8622244` (fresh baseline) | 11 | 28 | 2 | identical counts — row trusted |
| `5b61330` (final) | **1** | 38 | 2 | after fixes below |

## What was killed

The ten survivors were one cluster plus two stragglers:

- **Platform-config path helpers** (`user_config_dir`, `user_themes_dir`,
  `user_settings_path`) — pinned under a scoped `XDG_CONFIG_HOME`: config dir
  is `<root>/liberado`, themes one level deeper, settings.toml beside them.
  Each helper's `None` and `Some(Default::default())` replacements are caught
  by exact-path equality.
- **Settings round trip** — `save_theme_preference` creates the config tree,
  writes `settings.toml`, and trims the name; a second save overwrites.
  `load_ui_settings` reads it back through the real config dir, so the
  whole-body-to-`Default` mutant dies on a fixture file rather than a pure
  reimplementation seam.
- **`ThemeRegistry::is_empty` → `true`** — killed by the documented invariant
  that `new()` seeds dark/light/nord (`len() == 3`, not empty).
- **`Display for LoadError`** — must render "`path`: `message`"; the
  write-nothing replacement produces an empty string.

## Accepted survivor

| Location | Mutant | Why it stands |
|---|---|---|
| `lib.rs:590` | `is_empty()` → `false` | Every public constructor (`new`, `Default`) seeds three built-in themes; no reachable empty registry exists to distinguish the constant from the real check. |

## Harness notes

- The path helpers read `XDG_CONFIG_HOME` at call time via `dirs`. Tests point
  it at a tempdir under a process-wide mutex with restore-on-drop — same
  pattern as the coder-agent data-dir guards.
- Verification tooling note: the per-mutant kill script hardcodes its target
  crate; passing the wrong `-p` makes every mutation "survive" against another
  crate's tests. Always confirm the filter matches the crate under test.

The final run's `mutants.out/outcomes.json` is the authority for what remains;
the single survivor above is expected to persist indefinitely.
