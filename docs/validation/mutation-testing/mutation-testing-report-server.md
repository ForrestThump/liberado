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
and `session` (80%) but lower because telegram.rs (80/134 missed) has no unit test
coverage.

## Survivors by Module

| Module | Missed | Profile | Action |
|--------|:------:|---------|--------|
| `telegram.rs` | ~80 | `TelegramChatBridge` + `TelegramCommandContext` trait impls for live Telegram. ~60% of all survivors. No Telegram API key → cannot test. | Accept as infrastructure-gapped |
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
- **Telegram gap** dominates: the `TelegramChatBridge` and `TelegramCommandContext`
  together account for 60% of survivors. Only a live integration test with a real
  Telegram API key would exercise these.

## Remediation

Not recommended for mutation hardening. The server's testing surface is better served by:
- Tier 3 live conformance (per-path tests against the deployed daemon, per
  `live-conformance-suite.md`)
- Adding request-body seam tests at the provider boundary
- Expanding the T1 conformance suite (already at L1–L11)
