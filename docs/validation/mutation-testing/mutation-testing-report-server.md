# server — Mutation Testing Report

**Date:** 2026-08-23
**Status:** historical
**Authority:** evidence
**Ledger rows:** 2026-07-30 (markdown-era, `commit: null`), then three recorded campaigns:
`2153b7a` (158 survived), `551321a` (82), `0e39205` (76). Viable held at 345 across all three.

| Metric | 2026-07-30 | 2153b7a | 551321a | 0e39205 |
|--------|:---------:|:-------:|:-------:|:-------:|
| Viable | 184* | 345 | 345 | 345 |
| Caught | 50* | 182 | 257 | 263 |
| **Survived** | **134*** | **158** | **82** | **76** |

\* Different mutant-generation settings; not directly comparable. The comparable pair is the
last three rows, same tool and flags.

## What was killed

- **api/search** — the serde default page size (20).
- **latency** — `record()` end-to-end: events land in the JSONL journal.
- **shutdown** — grace env parsing (override / garbage / default) and signal-wait liveness.
- **cron_delivery** — the brief folds into the sticky conversation before the push; plain
  notify/proposal bypass the quiet wait; an active chat holds delivery for quiet_delay.
- **state** — reaction ring keeps exactly the newest 500 with oldest-first eviction; face
  compaction config derives per-model triggers; hot-swap resync retunes the default;
  NoTools refuses invocations; boot store opens under the resolved sessions root.
- **api/status** — active model prefers live provider, then snapshot, else none.
- **api/goals** — Windows extended-path prefixes strip for the wire; the diff cap is absolute,
  cuts on char boundaries, keeps an exact non-empty prefix (independent-size assertion so a
  shrunken cap cannot hide behind the shared constant).
- **api/chat** — both SSE verbs answer a disabled backend with an in-band `failed` event;
  attach/cancel distinguish 400/409/503; every `AgentEvent` maps onto the converged wire
  vocabulary; transcript nodes carry tool calls only when present; chat errors are JSON 500s.
- **lib.rs helpers** — WebUI dist override, port parse-or-default, CLI-over-config vault path
  with the both-empty hard error, delegation-mode tool surface (delegate + granted MCPs only),
  session-store root.
- **telegram** — the whole `CommandContext` surface: titles with blank filtering, parent
  resolution, prefix lookup, listing with untitled marker and `[goal]` suffix, status
  passthrough, theme-less behavior, mutators.

SSE bodies are read frame-wise under a deadline in tests: keep-alive comments mean these
bodies never reach EOF, and one early edit of this suite silently lost its GET leg — both
lessons are baked into the helpers now.

## Accepted survivors

### Equivalent by construction

| Location | Mutation | Why |
|---|---|---|
| `status.rs` models duplicate-insert | delete `!`, `==`→`!=` | The inserted duplicate is removed by the immediately following `sort()`+`dedup()`. |
| `state.rs` ring `>`→`>=` | fires one iteration early with `excess == 0`, a no-op drain. |
| `state.rs` ring `-`→`/` | Sends arrive one at a time, so `len/500` equals `len-500` whenever the branch runs. |
| `state.rs` NoTools `catalog` → `vec![]` | The body already returns `Vec::new()`. |
| `sticky.rs` persist guards ×3 | The match result feeds only `tracing::warn!`; no caller observes it. |
| `hooks.rs` seen_recently `<`→`<=` | Differs only when an entry's age equals the TTL to the nanosecond; unreachable deterministically. |
| `chat.rs` keep_alive → Default | axum's `KeepAlive::default()` is the same 15-second interval. |
| `telegram.rs` theme_names → `vec![]` | Identical to the production `Vec::new()`. |
| `lib.rs` face_tool_surface count guard `>` trio | Gates a `tracing::info!` only. |

### Needing a harness that does not exist yet

These are real behavior, killable with more machinery than this campaign could carry;
they are the natural next targets.

- **goals handler paths (11):** `goals_start` domain re-stamp, `spawn_return_handoff`'s
  park/finish/snapshot loop (4), `goals_rewind` coding-domain guard, `goals_diff` workspace
  guards and git-exit handling (4). Need a goal session with a real coding workspace wired
  through the hub (coder-agent pack registered in a test server state).
- **telegram bridge callbacks (6):** model/session browser rendering, spawn domain re-stamp,
  slash-arm dispatch, fork index math. Need a bridge over a populated store plus scripted
  broadcast events.
- **boot wiring (~27 in lib.rs):** `run`, `serve_with_drain`, `build_app_router`,
  `build_chat`/`build_coding_pack` stubs, `spawn_telegram_bot`, `wrap_cron_notifier`,
  `resolve_telegram_state`, `NotifySessionAlert`, `config_check`, the `explain_write`
  verdict printer (stdout-only today), guidance-source/vault/embedder lookups,
  `ChatPromptPreview::render`/`show_prompt`. These are the daemon's composition root;
  killing them means starting the real daemon against fixtures — its own campaign.

Two timing-sensitive survivors were deliberately *not* chased with sleeps-as-assertions:
`wait_for_quiet`'s immediate-return mutant is killed by a virtual-clock-free hold test, but
its internal sleep arithmetic (`-` variants live in `next_wait`, already covered) would need
the same treatment again if constants change.
