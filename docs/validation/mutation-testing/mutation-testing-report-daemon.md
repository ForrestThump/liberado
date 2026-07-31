# daemon — Mutation Testing Report

**Date:** 2026-07-30

| Metric | Value |
|--------|:-----:|
| Source files | 8 |
| Tests before | 38 |
| Tests after | 42 |
| Viable mutants | 67 |
| Caught | 34 |
| Catch rate | **50.7%** |
| False positives | — (see survivors below) |
| TIMEOUT | 16 (24%) |

## Overview

The daemon has the lowest catch rate of any Phase 4 crate so far. Two structural reasons:

1. **Event-driven test infrastructure.** Many mutants cause tests to hang (TIMEOUT) because
   the test waits on channels that never fire. The 16 timeouts are concentrated in
   `debounce.rs`, `vault_source.rs`, `react.rs`, `helpers.rs` (`archive_outcome_subdir`),
   and `proposals.rs` (`archive_terminal_proposal`) — all parts of the event pipeline
   where a no-op mutation breaks the message flow.

2. **Integration test shape.** 30 of 42 tests exercise the full daemon through
   `Daemon::run()` + `tokio::spawn(daemon.run(tx))` + channel recv. Mutants in the
   event source, debouncer, or reactor change observable behavior in ways that hang
   rather than assert failure.

Both are by design in a composition crate: the daemon's logic IS wiring. Mutation testing
exposes that the wiring is tested end-to-end rather than unit-testable in isolation.

## Survivors (17 missed + 16 TIMEOUT)

### IMPROVED — now caught (5 survivors)

| Survivor | Test |
|----------|------|
| `slugify` → `String::new()` / `"xyzzy".into()` | `slugify_collapses_non_alphanumeric` |
| delete `!` in `slugify` | `slugify_collapses_non_alphanumeric` |
| `stamp_local_time_if_needed` → `None` | `stamp_local_time_is_attached_for_cron_events` |
| `apply_approved_grant` with `()` | `handle_proposal_change_active_failed_not_expired_does_not_enter_expiry_path` |
| `&&`/`||`, `==`/`!=` in expiry logic | `handle_proposal_change_active_failed_not_expired_does_not_enter_expiry_path` |

### REMAINING — false positives / string constants (9)

| Line | Mutant | Reason |
|:----:|--------|--------|
| lib.rs:84 | `proposal_reap_interval` → `Default::default()` | Getter returning `Duration` field — semantically identical |
| lib.rs:97 | `user_timezone` → `None` / `Some(Default)` | Getter for optional field — any return works for the type |
| proposals.rs:369 | `proposal_reap_loop` with `()` | Spawned background task, no test awaits it |
| proposals.rs:407 | match guard `true` | `NotFound` error → empty dir, same observable result |
| react.rs:105-107 | delete `capabilities`/`profile`/`overrides` from `SessionGrant` | Fields are consumed by `hub.start_background`; no assertion on the grant after creation |
| react.rs:157 | `maybe_deliver_cron_result` with `()` | No test exercises cron delivery outcome assertions |
| types.rs:74 | `ReactionOutcome::label` → `""` / `"xyzzy"` | String label, display-only |
| vault_source.rs:40 | `EventSource::name` → `""` / `"xyzzy"` | String label, display-only |

### REMAINING — TIMEOUT (16, infrastructure-gapped)

These mutants hang the test by breaking the event pipeline. Fixing them requires
test-level timeouts on every `rx.recv()` or `hub.await_terminal()` call — a structural
change to the test infrastructure:

- `debounce.rs`: `observe`/`drain_ready`/`next_deadline` mutated → no events emitted
- `helpers.rs:61`: `archive_outcome_subdir` mutated → archive never completes
- `proposals.rs:76`: delete `!` in `handle_proposal_change` → logic inversion hangs
- `proposals.rs:253,342`: `archive_terminal_proposal` → no-op → archive incomplete
- `react.rs:43-44`: `!=`/`!` in `react` → routing inversion hangs pipeline
- `vault_source.rs:100,127,136`: `build_event` broken → no events emitted

## Patches Applied (4 new tests)

| Test | Caught |
|------|--------|
| `archive_outcome_subdir_maps_terminal_statuses` | Unit test for archive helper |
| `slugify_collapses_non_alphanumeric` | Unit test for slugify edges |
| `stamp_local_time_is_attached_for_cron_events` | Timezone stamp with/without path/zone |
| `handle_proposal_change_active_failed_not_expired_does_not_enter_expiry_path` | Expiry logic exact-match requirement |

## Remediation Path

The TIMEOUT survivors are the highest-value structural gap: they would cause the daemon
to hang in production. Fixing them requires adding a `test_timeout` wrapper to the daemon's
test infrastructure (e.g. `recv_or_timeout(rx, Duration::from_secs(5))` that fails fast
instead of hanging forever), then patching all 16 affected mutants.

This is scope beyond the current phase. Recommend a dedicated "test infrastructure
timeout hardening" pass after Phase 4 completes.
