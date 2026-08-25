# bootstrap — Mutation Testing Report

**Date:** 2026-08-25
**Status:** historical
**Authority:** evidence
**Scope:** `liberado-bootstrap`, full lib.

## Campaign history

| Ledger row | Survived | Caught | Viable |
|---|---:|---:|---:|
| markdown-era seeds | 20 | 29 | 49 |
| `82b28558` (2026-08-22) | 20 | 29 | 49 |
| `ce6286aa` fresh baseline | 20 | 29 | 49 |
| `68fd2a6d` (final) | **7** | 42 | 49 |

The two pre-campaign rows agreed exactly with a fresh run at zero drift —
counts were trustworthy this time, unlike several other crates.

## What was killed (13)

- **lib.rs** — the deepseek *fallback* arm selecting the actual deepseek
  profile (the primary-selection arm was already covered), factory construction
  when the selected profile's API key exists plus the role model applied over
  the profile default, `is_enabled` in both directions, model-only role
  overrides routing off the shared base provider (this also killed two of the
  three `\|\|→&&` guard mutants through precedence), trailing-slash capture-path
  dedup, and dispatch-pack assembly with configured providers (watch-only still
  returns `None`).
- **mcp_apply.rs** — `Display` carrying the rejection message, the live
  controller handing back the seeded catalog, blank-HTTP-url and blank-docker-
  image rejection guards each leaving the previous live set untouched.

Env-keyed tests use one dedicated variable under a file-local lock with a
save/restore guard (the `face_client` pattern): production construction paths
without process-global leaks.

## Accepted survivors (7)

| Location | Mutant | Why it stands |
|---|---|---|
| `lib.rs:233` | override guard → `true` | Only fires differently on an entirely empty `RoleOverride`; rebuilding from the same profile with nothing to apply yields a provider indistinguishable from `base.clone()`. |
| `lib.rs:236` | third `\|\|` → `&&` | Parses as `P \|\| M \|\| (T && R)`; diverges only when temperature or reasoning *alone* deviates, and `Provider` exposes neither — no public observation point. |
| `lib.rs:320` | pool settings → `Default` | Settings flow write-only into `ConnectionPool` policy; no getter anywhere on `McpRegistry`. |
| `lib.rs:335` | report sink → `None` | Stored in a private orchestrator field; observable only by driving live report delivery. |
| `lib.rs:350` ×3 | caps map emptied / junk keys | `with_session_profile_caps` stores into daemon-private state; reaction-time narrowing needs a full dispatcher stack to observe. |

The honest label for five of these is **unobservable-through-public-surface**
rather than equivalent: a getter or integration harness would kill them. They
are wiring whose failure modes live behind other crates' encapsulation.

## Notes

- `lib.rs:451` taught the line-drift lesson again in reverse: three mutants
  share one source line, and reading the survivor *name* without the diff
  column misidentifies which comparison changed. Read `mutants.out/diff/*.diff`.
- Rust precedence turned the middle `\|\|→&&` mutants into different predicates
  than a naive left-to-right substitution suggests; evaluating the parsed form
  per fixture predicted kills correctly once done.
