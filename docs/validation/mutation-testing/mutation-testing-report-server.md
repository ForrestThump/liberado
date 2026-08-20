# server — Mutation Testing Report

**Date:** 2026-07-30

| Metric | Value |
|--------|:-----:|
| Source files | 14 |
| Tests | 56 |
| Viable mutants | 184 |
| Caught | 50 |
| **Catch rate** | **27.2%** |
| Unviable | 42 |
| TIMEOUT | 0 |

## Overview

The server is a composition crate that wires together daemon, dispatcher, orchestrator,
main-agent, session-store, mcp, notifier, and 15+ other crate types. Most of its code
is glue. The 27% catch rate is expected for this profile — comparable to `config` (52%)
and `session` (80%) but lower because telegram.rs (80/134 missed) had no unit test
coverage at the time of this run.

> **Update (2026-08-19):** the "no API key → cannot test" note below is **stale**.
> `crates/server/src/telegram.rs` now carries 9 mock-based tests (`MockProvider` from
> `liberado_provider`, plus `PendingProvider`/`HangOnceProvider`) that run with no
> Telegram API key and no network — in-memory `SessionStore` + `AppState::for_test`.
> They cover the lifecycle surface (free-form turns, /stop, /model scoping, the
> unanswered-turn note, /help). A re-run of the mutant campaign on telegram.rs would
> move its current catch rate well above the 27% crate figure.

## Survivors by Module

| Module | Missed | Profile | Action |
|--------|:------:|---------|--------|
| `telegram.rs` | ~80 | `TelegramChatBridge` + `TelegramCommandContext` trait impls for live Telegram. ~60% of all survivors at the time of this run. | **Revisit with mock-based tests** — see Overview update; the no-API-key blocker is gone |
| `lib.rs` | ~20 | `build_chat`, `explain_write`, `config_check` — daemon boot path, CLI commands | Partial: low-hanging operators in `build_chat` |
| `api/goals.rs` | 5 | SSE event conversion, goal validation | Accept |
| `state.rs` | 6 | `NoTools` trait impl (thin), `reaction_tx` operator | Accept |
| `cron_delivery.rs` | 5 | `ChatDeliveringNotifier` trait impls | Accept |
| `api/chat.rs` | 3 | SSE event, error response, chat message handler | Accept |
| `api/status.rs` | 3 | Getter, negation | Accept |
| `hooks.rs` | 1 | `IdempotencyCache::seen_recently` operator | Accept |
| `api/search.rs` | 1 | Constant function | Accept |
| `latency.rs` | 1 | Trait impl | Accept |

## Key Takeaways

- **No timeouts** — the `--timeout 5.0 --minimum-test-timeout 90` flags successfully
  prevented hangs.
- **42 unviable mutants** — indicates ~22% of the crate is not exercised by any test,
  consistent with a composition crate.
- **Telegram gap** dominated: the `TelegramChatBridge` and `TelegramCommandContext`
  accounted for 60% of survivors at this run. That is no longer an infrastructure blocker —
  see the Overview update: the bridge is tested today via scripted providers that need no
  live Telegram API.

## Remediation

Not recommended for mutation hardening. The server's testing surface is better served by:
- Tier 3 live conformance against the deployed daemon, per the
  [conformance runbook](../../impl/live-conformance.md)
- Adding request-body seam tests at the provider boundary
- Expanding the T1 conformance suite (already at L1–L11)
