---
kind: finding
status: active
authority: evidence
domain: coding-harness
canonical_for: harness-baseline-0.7-c3
open_items: true
---

# 0.7 / C3 — controlled cross-harness baseline

**Status**: Evidence recorded 2026-08-13. One sequential run, one task, three
harnesses. Not closed: Hermes was not run, n = 1 so there is no p50/p95, and
the pinned forks were not given new MVL emitters.

**Who this is for**: anyone picking the first cost lever (0.9) or changing the
coding-pack turn budget, repair excerpt, or ship bar.

**Run write-up**: `C:\Users\Shiloh\Code\life-os-harness-compare4\COMPARE.md`.

Related: [`harness-study-2026-08.md`](harness-study-2026-08.md),
[`f12-compare3-harness-failures-2026-08.md`](f12-compare3-harness-failures-2026-08.md).

---

## Pins

| Pin | Value |
|---|---|
| Task | Backlog **B1** — `ExecuteDirect` explicit delivery destination |
| Commit | `0ac59ca` (after #156 F12 and #157 repair excerpt / 50-turn budget) |
| Model | `deepseek/deepseek-v4-flash` via OpenRouter |
| Thinking | high on all three |
| Temperature | 0.1 |
| Caps | Liberado 50 turns × 3 attempts; deepagents recursion 80; pi uncapped |
| Prompts / tools | native per harness |
| Repeats | **none** |

Hermes is in the 0.7 text. This baseline uses the living three from compares
1–3 (Liberado, pi, deepagents).

---

## Scoreboard (what 0.7 asked for)

| Metric | Liberado | pi | deepagents |
|---|---|---|---|
| Ship-gate pass | **0 / 1** (`cargo-test` 101) | no Liberado ship bar | no Liberado ship bar |
| Merge-ready | **no** | **no** (timeout before tests) | **no** (stopped mid-edit) |
| Wall clock | 73 min 29 s | 17 min 8 s | 15 min 39 s |
| Turns | 151 (52+47+52) | 88 | 40 |
| Cost (Flash rates in `config.example`) | **~$0.21** | unknown (no usage in session JSONL) | **~$0.25** |
| Cost per accepted result | n/a — **0 accepted** | n/a | n/a |
| p50 / p95 latency | n = 1 — the wall times above are the only sample | same | same |
| Human repair still needed | yes — test 101 + design is not the requested field | yes — finish tests after the API timeout | yes — wire the new field |

Rates used: `input 0.14`, `cached_input 0.0028`, `output 0.28` USD / MTok.
Liberado input treated as inclusive of cache (new ≈ 0.50 M, cache ≈ 6.95 M,
output ≈ 0.42 M). Deepagents reported 1.64 M input, 0 cache, 62 k output.

---

## Trace-linked failure classes

| Class | Who | Evidence |
|---|---|---|
| `turn_budget` | Liberado attempts 0 and 2 | Report: “exceeded its 52-turn budget”. Pin is 50. |
| `progress_guard` | Liberado attempt 0 | 40 inspect calls with no mutation; tools then refused. |
| `command_failed` / compile | Liberado attempt 0 | E0025/E0062 duplicate `relevant_mcps`. #157 excerpt named it. Attempt 1 fixed it. |
| `command_failed` / tests | Liberado attempts 1–2 | `cargo-test` 101. Last-40-line excerpt is a **passing** `wire` crate (61 ok). Failing crate not named. |
| `wrong_design` | Liberado | Inferred `chat-delegate-` instead of adding `Delivery` on `ExecuteDirect`. Task forbade a blanket relay append and asked for an explicit destination. |
| `provider_timeout` | pi | Last assistant text: `Connect timeout, please try again later.` Tree dirty; `PI_EXIT=0`. |
| `recursion_limit` | deepagents | 40 completions, exit 1. Last completion still planning the orchestrator edit. |
| `unfinished_enum_change` | deepagents | `delivery` on the enum; match arms and constructors not updated. |

---

## What this does *not* support

- **Do not treat 0/3 as a model ranking.** Same Flash. Different loops and
  different stop rules.
- **Do not compute p50/p95** from this file. n = 1 per harness.
- **Do not close 0.7.** Hermes, fork MVL emitters, and repeats are still open.
- **Do not raise Liberado `max_turns` from this run alone.** 50 was short
  (hit twice). pi needed 88 and still did not finish tests. 80 is the next
  number to try on a later change, not a claim that 80 would have shipped B1.

---

## What it does support

1. Liberado’s control plane (catalog, thinking tokens, compile, rustc excerpt)
   works on a live Flash coding task.
2. Liberado still loses on **finish**: workspace `cargo test` after a
   multi-crate change.
3. Last-N lines of a workspace test log is the wrong excerpt shape. Prefer
   FAILED / error lines, or the first failing package.
4. Adding a field to `ExecuteDirect` has a constructor blast radius. Liberado
   avoided it and missed the item. pi took it and ran out of provider time.
5. Tool-output offload (harness-study lever 1) is still the leading cost
   hypothesis: Liberado re-sent ~7.5 M input tokens, 93% of them cached, and
   still paid for a 73-minute loop. This baseline does not yet prove the lever.

---

## Artifact paths

```
C:\Users\Shiloh\Code\life-os-harness-compare4\
  pins.txt
  task.txt
  COMPARE.md
  out\liberado\traces\
  out\pi\session.jsonl
  out\deepagents\run.mvl.jsonl
```
